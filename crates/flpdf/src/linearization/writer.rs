//! qpdf correspondence: QPDFWriter.cc linearized write path split from the standard writer.
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
//! Part 2       | hint stream object (filtered according to stream-data policy)
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
//! qpdf writes the hint object once between two layout passes:
//!
//! 1. **Pass 1**: omit the reserved hint object and collect the virtual offsets
//!    and byte lengths for the rest of the file.
//! 2. **Build the hint object**: fill all hint tables from those pass-1 values,
//!    encode the payload, and frame/encrypt the complete indirect object once.
//! 3. **Pass 2**: write the final file and splice that exact hint-object buffer
//!    into the reserved slot. Fixed-width Part 1 padding keeps the downstream
//!    offsets aligned with the values encoded in the hint tables.
//!
//! # Scope
//!
//! Back-patching the placeholder values is the responsibility of a later step.
//! This module returns `LinearizedOffsets` containing all information required
//! for that step.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, Write};
use std::path::Path;

use crate::linearization::hint_page::{bits_needed, PageOffsetHintTable};
use crate::linearization::hint_shared::SharedObjectHintTable;
use crate::linearization::hint_stream::{encode_hint_stream, OutlineHintTable};
use crate::linearization::part1::{Part1Bytes, Part1Placeholders};
use crate::linearization::plan::{
    collect_direct_refs, ContainerPart, LinearizationPlan, RoutedObjStmBatch,
};
use crate::linearization::renumber::{ObjStmRelocation, RenumberMap, SecondHalfContainerAnchor};
use crate::pipeline::stdio_file::StdioBuffer;
use crate::writer::encrypted_strings::EncryptedStringEmitter;
use crate::writer::object_streams::{
    emit_objstm_body_from_handles_with_writer, planner_config_from_options,
    wrap_objstm_body_as_handle,
};
use crate::writer::{
    decrement_progress_event, effective_pdf_version_and_ext, effective_stream_policy,
    inject_adbe_extension, report_progress_event, serialize::xref_stream, strip_adbe_extension,
    CompressStreams, NewlineBeforeEndstream, ObjectWriterEmission, WriterOptions, WriterResult,
};
use crate::{Object, ObjectHandle, ObjectRef, Pdf, Result};

const EBADF_ERRNO: i32 = 9;

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
/// Built once, before the two layout passes, from the Part-tagged
/// [`crate::linearization::plan::ObjStmBatchPlan`].  The contained-object set
/// and per-container membership are **stable across the two passes** (only the
/// surrounding byte offsets shift), which keeps both passes on the same layout.
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
        options: &WriterOptions,
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

/// Build the ObjStm container stream object for one scheduled container from
/// the live member handles and apply qpdf's global structural-stream
/// compression policy.
fn append_objstm_container_object<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    container: &ObjStmContainer,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
    removed_refs: &BTreeSet<ObjectRef>,
    filtered: bool,
    encrypt_ctx: Option<&crate::writer::EncryptionContext>,
) -> Result<usize> {
    let map = |object_ref| {
        renumber.new_for_original(object_ref).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "linearization writer: ObjStm member reference {object_ref} has no renumber entry"
            ))
        })
    };
    let mut members: Vec<(ObjectRef, ObjectHandle)> = Vec::with_capacity(container.members.len());
    for &(orig, new_ref) in &container.members {
        let handle = pdf.get_object_handle(orig);
        pdf.resolve(&handle)?;
        // qpdf warns and writes null when a malformed source stream is routed
        // into an object stream (`QPDFWriter.cc:1714-1721`). Keep that edge at
        // the canonical handle boundary rather than materializing a legacy
        // `Object::Stream` that cannot be a valid ObjStm member.
        let handle = if handle.as_stream_dict().is_some() {
            ObjectHandle::null()
        } else {
            handle
        };
        members.push((new_ref, handle));
    }
    let body = emit_objstm_body_from_handles_with_writer(
        &members,
        &mut |out, _member_index, _object_ref, handle| {
            handle.write_object_with_ref_map_and_removed(out, &map, removed_refs)
        },
    )?;
    let compress = if filtered {
        CompressStreams::Yes
    } else {
        CompressStreams::No
    };
    let (stream_handle, data) = wrap_objstm_body_as_handle(&body, compress, None)?;
    let stream_dict = stream_handle.as_stream_dict().ok_or_else(|| {
        // cov:ignore-start: wrap_objstm_body_as_handle always returns a stream handle.
        crate::Error::Internal("linearization ObjStm wrapper produced a non-stream handle".into())
        // cov:ignore-end
    })?; // cov:ignore: wrap_objstm_body_as_handle always returns a stream handle.
    let object_ref = ObjectRef::new(container.container_new_num, 0);
    // PDF encryption applies to the ObjStm container stream as one stream
    // object. The member objects remain plaintext inside that encrypted
    // payload; encrypting them individually would not match qpdf or the PDF
    // object-stream encryption rules (`QPDFWriter.cc:1782-1800`).
    if let Some(ctx) = encrypt_ctx {
        let mut payload_length = data.len();
        crate::writer::adjust_aes_stream_length(&mut payload_length, ctx, true)?;
        stream_dict.replace_key(
            b"/Length",
            ObjectHandle::integer(i64::try_from(payload_length).unwrap_or(i64::MAX)),
        )?;
    }

    // qpdf writes ObjStm keys in the fixed order Type/Length/Filter/N/First.
    // The values themselves are emitted by the canonical handle serializer;
    // only the surrounding order and object-stream framing remain raw layout.
    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} 0 obj\n", container.container_new_num).as_bytes());
    bytes.extend_from_slice(b"<< /Type ");
    stream_dict.get_key(b"/Type").write_object(bytes)?;
    bytes.extend_from_slice(b" /Length ");
    stream_dict.get_key(b"/Length").write_object(bytes)?;
    if filtered {
        bytes.extend_from_slice(b" /Filter ");
        stream_dict.get_key(b"/Filter").write_object(bytes)?;
    }
    bytes.extend_from_slice(b" /N ");
    stream_dict.get_key(b"/N").write_object(bytes)?;
    bytes.extend_from_slice(b" /First ");
    stream_dict.get_key(b"/First").write_object(bytes)?;
    bytes.extend_from_slice(b" >>");
    if let Some(ctx) = encrypt_ctx {
        crate::writer::write_stream_payload_with_pipeline(
            bytes,
            &data,
            NewlineBeforeEndstream::Never,
            object_ref,
            ctx,
            true,
            None,
        )?;
    } else {
        crate::writer::serialize::write_stream_payload(bytes, &data, NewlineBeforeEndstream::Never);
    }
    bytes.extend_from_slice(b"\nendobj\n");
    Ok(offset)
}

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Byte offsets and derived values returned by the internal `write_linearized`
/// implementation.
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

    /// Full byte length of the hint stream indirect object — corresponds to
    /// `/H[1]` in the param dict.
    ///
    /// This spans the complete `N 0 obj … endobj` representation, including
    /// the object header, stream dictionary, encoded payload, stream framing,
    /// and `endobj`. It is distinct from the hint stream dictionary's
    /// `/Length`, which counts only the encoded payload.
    pub hint_stream_length: usize,

    /// New object number assigned to the first-page page object — corresponds
    /// to `/O` in the param dict.  Typically `2` (first Part-2 object).
    pub first_page_object_new_num: u32,

    /// Byte offset immediately after the last byte of Annex F Part 3 (the
    /// first-page body section, `Plan.part2_objects`).  Corresponds to `/E`.
    pub end_of_first_page_offset: usize,

    /// Byte offset of the Part 6 cross-reference table (`xref` keyword).
    /// Used internally for layout diagnostics.
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

/// Append `N G obj\n<object>\nendobj\n` to `bytes` and return the offset of the
/// `N G obj` header (i.e. the start of the object).
///
/// When `encrypted_string_emitter` is `Some`, strings are encrypted from the
/// writer's per-object emission state without mutating `object`. The
/// `/Encrypt` dictionary never reaches this helper: [`do_write_pass`] emits it
/// directly through
/// [`write_encryption_dictionary`](crate::writer::encrypted_strings::write_encryption_dictionary)
/// so it remains plaintext.
fn append_object(
    bytes: &mut Vec<u8>,
    new_ref: ObjectRef,
    object: &ObjectHandle,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    encrypted_string_emitter: Option<&mut EncryptedStringEmitter>,
) -> Result<usize> {
    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes());
    if let Some(emitter) = encrypted_string_emitter {
        emitter.write_handle_object_with_ref_map(
            bytes,
            new_ref,
            None,
            object,
            false,
            map,
            removed_refs,
        )?; // cov:ignore: canonical handle emission only errors for an invalid source graph.
    } else {
        object.write_object_with_ref_map_and_removed(bytes, map, removed_refs)?;
    }
    bytes.extend_from_slice(b"\nendobj\n");
    Ok(offset)
}

/// Prepend a qpdf Crypt filter to a live stream dictionary without
/// materializing a legacy dictionary.
fn prepend_crypt_filter_to_handle_entries(
    entries: &mut BTreeMap<Vec<u8>, ObjectHandle>,
    cf_name: &[u8],
) -> Result<()> {
    let crypt_decode_parms = ObjectHandle::dictionary(vec![
        (
            b"/Type".to_vec(),
            ObjectHandle::name(b"CryptFilterDecodeParms".to_vec()),
        ),
        (b"/Name".to_vec(), ObjectHandle::name(cf_name.to_vec())),
    ]);
    let existing_filter = entries.remove(b"/Filter".as_slice());
    let existing_decode_parms = entries.remove(b"/DecodeParms".as_slice());

    match existing_filter {
        None => {
            entries.insert(b"/Filter".to_vec(), ObjectHandle::name(b"Crypt".to_vec()));
            entries.insert(b"/DecodeParms".to_vec(), crypt_decode_parms);
        }
        Some(filter) => {
            if let Some(name) = filter.try_as_name()? {
                entries.insert(
                    b"/Filter".to_vec(),
                    ObjectHandle::array(vec![
                        ObjectHandle::name(b"Crypt".to_vec()),
                        ObjectHandle::name(name),
                    ]),
                );
                entries.insert(
                    b"/DecodeParms".to_vec(),
                    ObjectHandle::array(vec![
                        crypt_decode_parms,
                        existing_decode_parms.unwrap_or_else(ObjectHandle::null),
                    ]),
                );
            } else if let Some(mut filters) = filter.try_as_array()? {
                let chain_len = filters.len();
                let mut new_filters = Vec::with_capacity(chain_len + 1);
                new_filters.push(ObjectHandle::name(b"Crypt".to_vec()));
                new_filters.append(&mut filters);

                let mut new_decode_parms = Vec::with_capacity(chain_len + 1);
                new_decode_parms.push(crypt_decode_parms);
                match existing_decode_parms {
                    None => new_decode_parms.extend((0..chain_len).map(|_| ObjectHandle::null())),
                    Some(params) => {
                        if let Some(params) = params.try_as_array()? {
                            new_decode_parms.extend(
                                params
                                    .into_iter()
                                    .chain(std::iter::repeat_with(ObjectHandle::null))
                                    .take(chain_len),
                            );
                        } else {
                            new_decode_parms.push(params);
                            new_decode_parms.extend((1..chain_len).map(|_| ObjectHandle::null()));
                        }
                    }
                }
                entries.insert(b"/Filter".to_vec(), ObjectHandle::array(new_filters));
                entries.insert(
                    b"/DecodeParms".to_vec(),
                    ObjectHandle::array(new_decode_parms),
                );
            } else {
                entries.insert(b"/Filter".to_vec(), ObjectHandle::name(b"Crypt".to_vec()));
                entries.insert(b"/DecodeParms".to_vec(), crypt_decode_parms);
            }
        }
    }
    Ok(())
}

/// Append one live-handle body object in output-number space.
#[allow(clippy::too_many_arguments)]
fn append_body_object(
    bytes: &mut Vec<u8>,
    new_ref: ObjectRef,
    original_ref: ObjectRef,
    object: &ObjectHandle,
    options: &WriterOptions,
    encrypt_ctx: Option<&crate::writer::EncryptionContext>,
    encrypted_string_emitter: Option<&mut EncryptedStringEmitter>,
    renumber: &RenumberMap,
    removed_refs: &BTreeSet<ObjectRef>,
    content_normalize_refs: &BTreeSet<ObjectRef>,
) -> Result<usize> {
    let map = |object_ref| {
        renumber.new_for_original(object_ref).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "linearization writer: reference {object_ref} has no renumber entry"
            ))
        })
    };

    if object.as_stream_dict().is_none() {
        return append_object(
            bytes,
            new_ref,
            object,
            &map,
            removed_refs,
            encrypted_string_emitter,
        );
    }

    let (dict, data, mut refiltered) =
        crate::writer::plain::body::canonical_stream_output_for_linearization(
            object,
            options,
            options.content_normalization && content_normalize_refs.contains(&original_ref),
        )?; // cov:ignore: LLVM maps this covered stream-output call terminator to a zero-count continuation region
    let mut entries = dict.try_as_dictionary()?.unwrap_or_default();
    let payload_ctx = encrypt_ctx.filter(|ctx| new_ref != ctx.encrypt_ref);
    let cleartext_metadata = payload_ctx
        .is_some_and(|ctx| !ctx.encrypt_metadata && ctx.metadata_ref == Some(original_ref));
    if cleartext_metadata {
        prepend_crypt_filter_to_handle_entries(&mut entries, b"Identity")?;
        // The final dictionary now carries an explicit `/Crypt` stage, so it
        // is no longer the lone `/FlateDecode` shape that selects qpdf's
        // refiltered key ordering.
        refiltered = false;
    }

    let mut payload_length = data.len();
    if let Some(ctx) = payload_ctx.filter(|_| !cleartext_metadata) {
        crate::writer::adjust_aes_stream_length(&mut payload_length, ctx, true)?;
    }
    entries.insert(
        b"/Length".to_vec(),
        ObjectHandle::integer(i64::try_from(payload_length).unwrap_or(i64::MAX)),
    );
    let dict = ObjectHandle::dictionary(entries.into_iter().collect());

    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes());
    if let Some(emitter) = encrypted_string_emitter {
        emitter.write_handle_stream_dict_with_ref_map(
            bytes,
            new_ref,
            None,
            &dict,
            crate::writer::encrypted_strings::StreamDictOptions::new(false, refiltered, true),
            &map,
            removed_refs,
            None,
        )?; // cov:ignore: canonical stream-dictionary emission only errors for an invalid source graph.
    } else {
        dict.write_stream_body_with_ref_map_and_removed(bytes, refiltered, &map, removed_refs)?;
    }

    if let Some(ctx) = payload_ctx.filter(|_| !cleartext_metadata) {
        crate::writer::write_stream_payload_with_pipeline(
            bytes,
            &data,
            NewlineBeforeEndstream::Never,
            new_ref,
            ctx,
            true,
            None,
        )?; // cov:ignore: stream payload encryption is a validated in-memory writer boundary.
    } else {
        crate::writer::serialize::write_stream_payload(bytes, &data, NewlineBeforeEndstream::Never);
    }
    bytes.extend_from_slice(b"\nendobj\n");
    Ok(offset)
}

#[allow(clippy::too_many_arguments)]
fn append_body_object_for_ref<R: Read + Seek>(
    bytes: &mut Vec<u8>,
    pdf: &mut Pdf<R>,
    new_ref: ObjectRef,
    original_ref: ObjectRef,
    options: &WriterOptions,
    encrypt_ctx: Option<&crate::writer::EncryptionContext>,
    encrypted_string_emitter: Option<&mut EncryptedStringEmitter>,
    renumber: &RenumberMap,
    removed_refs: &BTreeSet<ObjectRef>,
    content_normalize_refs: &BTreeSet<ObjectRef>,
) -> Result<usize> {
    let object = pdf.get_object_handle(original_ref);
    pdf.resolve(&object)?;
    append_body_object(
        bytes,
        new_ref,
        original_ref,
        &object,
        options,
        encrypt_ctx,
        encrypted_string_emitter,
        renumber,
        removed_refs,
        content_normalize_refs,
    )
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
/// place by [`patch_part1_xref`] once the final pass has every offset. Because
/// the block byte length is invariant, no downstream offset shifts when the
/// complete hint object is spliced between the two passes.
///
/// The first-page trailer includes `/Info` (when present), `/Root`, `/Size`,
/// `/Prev`, and `/ID` — matching qpdf's key order and content for linearized
/// PDFs.  The `/Prev` value is written as a left-justified decimal integer
/// padded on the right with spaces to [`PREV_PLACEHOLDER_WIDTH`] bytes so it
/// can be back-patched in-place once the Part 6 xref offset is known.  When
/// `encrypt_ctx` is `Some`, `/Encrypt {N} 0 R` is written immediately after
/// `/ID` — qpdf's `writeTrailer` writes `/ID` first, then, for every trailer
/// form except `t_lin_second` (the main/second-half trailer),
/// ` /Encrypt {objid} 0 R` (QPDFWriter.cc:1224-1231). Verified empirically against qpdf
/// 11.9.0 (`qpdf --linearize --static-id --static-aes-iv --encrypt "" "" 128
/// --use-aes=y`), whose first-page trailer ends `... /ID [<...><...>]
/// /Encrypt 4 0 R >>`.
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
    source_trailer: &ObjectHandle,
    canonical_entries: &[(Vec<u8>, Vec<u8>)],
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    id_writer: Option<crate::object::ReborrowableIdWriter>,
    encrypt_ctx: Option<&crate::writer::EncryptionContext>,
) -> Result<(usize, std::ops::Range<usize>, Part1XrefPatch)> {
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

    // First-page trailer for Part 1. qpdf emits the live trimmed trailer keys
    // in decoded-name order, inserts `/Prev` immediately after `/Size`, and
    // appends `/ID` and `/Encrypt` after the ordinary keys. The values in
    // `canonical_entries` came from the live ObjectHandle graph; raw bytes are
    // limited to this fixed linearization framing and its back-patch field.
    bytes.extend_from_slice(b"trailer << ");

    let mut entries = canonical_entries.to_vec();
    if let Some(info_ref) = info_new_ref {
        entries.push((
            b"/Info".to_vec(),
            format!("{} {} R", info_ref.number, info_ref.generation).into_bytes(),
        ));
    }
    entries.push((
        b"/Root".to_vec(),
        format!(
            "{} {} R",
            catalog_new_ref.number, catalog_new_ref.generation
        )
        .into_bytes(),
    ));
    entries.push((
        b"/Size".to_vec(),
        total_object_count.to_string().into_bytes(),
    ));
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut prev_value_range = None;
    for (key, value) in entries {
        bytes.push(b'/');
        crate::object::write_name_escaped(bytes, key.strip_prefix(b"/").unwrap_or(&key));
        bytes.push(b' ');
        bytes.extend_from_slice(&value);
        if key == b"/Size" {
            // `/Prev` placeholder: left-justified 0, space-padded to the
            // fixed qpdf field width so the final xref offset is patched in
            // place without shifting any body bytes.
            bytes.extend_from_slice(b" /Prev ");
            let prev_value_start = bytes.len();
            let placeholder = format!("{:<PREV_PLACEHOLDER_WIDTH$}", 0);
            bytes.extend_from_slice(placeholder.as_bytes());
            prev_value_range = Some(prev_value_start..bytes.len());
        } else {
            bytes.push(b' ');
        }
    }

    // No separator space before `/ID`: the fixed-width `/Prev` placeholder's
    // trailing pad already separates the value, exactly as qpdf writes it.
    bytes.extend_from_slice(b"/ID ");
    let id_value = source_trailer.try_get_key(b"/ID")?;
    match id_writer {
        Some(write_id) => write_id(bytes),
        None => id_value.write_id_value_with_ref_map(bytes, map, removed_refs)?,
    }

    // /Encrypt — reference to the `/Encrypt` dictionary object, written right
    // after `/ID` (qpdf `writeTrailer` writes `/ID` first, then — for every
    // trailer form except `t_lin_second`, the main/second-half trailer —
    // ` /Encrypt {objid} 0 R`, QPDFWriter.cc:1224-1231). The main (Part-6)
    // trailer is produced by `write_main_xref_and_trailer`, a separate
    // function that never receives `encrypt_ctx`, so that omission is
    // structural rather than a runtime branch here.
    if let Some(ctx) = encrypt_ctx {
        bytes.extend_from_slice(
            format!(
                " /Encrypt {} {} R",
                ctx.encrypt_ref.number, ctx.encrypt_ref.generation
            )
            .as_bytes(),
        );
    }

    bytes.extend_from_slice(b" >>");
    // Per linearized PDF convention (ISO 32000-1 Annex F and qpdf practice),
    // the Part 1 first trailer's startxref value is always 0.  The main xref
    // at the end of the file (Part 6) carries the real byte offset in its own
    // trailing startxref, so readers that follow the tail-startxref path are
    // unaffected.  qpdf uses 0 here to signal "this is the first trailer of a
    // linearized file"; we adopt the same convention for byte-identical output.
    bytes.extend_from_slice(b"\nstartxref\n0\n%%EOF\n");

    let prev_value_range = prev_value_range.ok_or_else(|| {
        // cov:ignore-start: /Size is inserted unconditionally above.
        crate::Error::Unsupported(
            "linearization first trailer has no /Size entry for /Prev patch".to_string(),
        )
        // cov:ignore-end
    })?; // cov:ignore: /Size is inserted unconditionally above.
    Ok((xref_offset, prev_value_range, patch))
}

/// Overwrite the classic first-page xref subsection's placeholder entry block
/// in place, now that every covered object offset is known.
///
/// The subsection covers objects `[start_num, start_num + count)` — the whole
/// first-page section — all of which are plain indirects on the classic path,
/// so the encoder needs only the final `xref_offsets` map.  Because the block
/// was emitted at its final byte length, this is a pure in-place patch: no
/// offset shifts occur when the complete hint object is spliced.
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
#[allow(clippy::too_many_arguments)]
fn write_main_xref_and_trailer(
    bytes: &mut Vec<u8>,
    xref_offsets: &BTreeMap<u32, usize>,
    param_slot: u32, // /Size of the main subsection — covers objects [0, param_slot)
    first_page_xref_offset: usize,
    source_trailer: &ObjectHandle,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
    id_writer: Option<crate::object::ReborrowableIdWriter>,
) -> Result<(usize, usize)> {
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
    bytes.extend_from_slice(b"/ID ");
    let id_value = source_trailer.try_get_key(b"/ID")?;
    match id_writer {
        Some(write_id) => write_id(bytes),
        None => id_value.write_id_value_with_ref_map(bytes, map, removed_refs)?,
    }
    bytes.extend_from_slice(b" ");
    bytes.extend_from_slice(b">>");
    bytes.extend_from_slice(format!("\nstartxref\n{}\n%%EOF\n", first_page_xref_offset).as_bytes());

    Ok((xref_start, xref_first_entry_offset))
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
    /// overwrites the whole region with the real encoded object plus trailing
    /// space padding, so the next object's offset is independent of the payload
    /// length and the later hint-object splice is unaffected.
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
    /// Live trailer entries serialized through the canonical ObjectHandle
    /// graph. Writer-owned keys are added separately by the xref serializer.
    canonical_entries: Vec<(Vec<u8>, Vec<u8>)>,
    /// Trailer `/Encrypt` reference on the first-page xref stream. The main
    /// linearization xref stream intentionally omits this (`t_lin_second`).
    encrypt: Option<ObjectRef>,
    /// Highest object number (sizes field 2 alongside the max offset).
    max_id: u32,
    /// Largest object-stream member index (sizes field 3 of `/W`).
    max_ostream_index: u64,
    /// Whether the generated xref stream uses predictor + Flate filtering.
    filtered: bool,
}

/// Return the live trailer entries that the linearization xref dictionary may
/// carry in addition to its writer-owned structural fields.
///
/// qpdf's `getTrimmedTrailer` walks the live trailer handle, while the
/// linearization writer supplies `/Info`, `/Root`, `/Size`, `/Prev`, `/ID`, and
/// `/Encrypt` at the xref/trailer boundary. Keep those generated keys out of
/// this list so they cannot be duplicated. Values are serialized through the
/// canonical handle graph and mapped into output-number space; only the xref
/// dictionary's fixed framing and key order remain raw layout.
///
/// A value that is itself an indirect handle is never dereferenced here: qpdf's
/// `writeTrailer` unparses each surviving key through `unparseChild`
/// (`QPDFWriter.cc:1143-1155`), which branches solely on `child.isIndirect()`
/// and, when true, writes the renumbered `"N 0 R"` token without ever
/// inspecting what that reference resolves to -- an indirect stream target is
/// no exception, so this mirrors that check before falling back to the
/// generic handle-graph unparse used for direct values. Same split as the
/// plain writer's sibling `canonical_trailer_entries`
/// (`crates/flpdf/src/writer/plain/plan.rs`).
fn canonical_linearization_trailer_entries(
    trailer: &ObjectHandle,
    map: &dyn Fn(ObjectRef) -> Result<ObjectRef>,
    removed_refs: &BTreeSet<ObjectRef>,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let entries = trailer.try_as_dictionary()?.unwrap_or_default();
    let mut serialized = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if matches!(
            key.as_slice(),
            b"/ID"
                | b"/Encrypt"
                | b"/Info"
                | b"/Prev"
                | b"/Root"
                | b"/Size"
                | b"/Type"
                | b"/F"
                | b"/FFilter"
                | b"/FDecodeParms"
                | b"/W"
                | b"/Index"
                | b"/Length"
                | b"/Filter"
                | b"/DecodeParms"
                | b"/XRefStm"
        ) {
            continue;
        }
        let removed = value
            .object_ref()
            .is_some_and(|object_ref| object_ref.number == 0 || removed_refs.contains(&object_ref))
            || value.as_reference().is_some_and(|object_ref| {
                object_ref.number == 0 || removed_refs.contains(&object_ref)
            });
        if removed || value.try_is_null()? {
            continue;
        }
        let mut value_bytes = Vec::new();
        if let Some(object_ref) = value.object_ref() {
            let mapped = map(object_ref)?;
            value_bytes.extend_from_slice(mapped.to_string().as_bytes());
        } else {
            value.write_object_with_ref_map_and_removed(&mut value_bytes, map, removed_refs)?;
        }
        serialized.push((key, value_bytes));
    }
    Ok(serialized)
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
///     `[<0×32><0×32>]` installed here so pass 1 (which only measures object
///     byte lengths) emits a fixed-width `/ID`. The real two-level
///     identifier cannot be known until the bytes exist, so it is computed from
///     a digest over a reconstruction of qpdf's first write pass. The classic
///     (stream-free) path then **direct-writes** that identifier at every `/ID`
///     site in the final pass (qpdf's 2-pass scheme), so the placeholder never
///     reaches the finished output. The ObjStm / xref-stream path instead
///     back-patches the placeholder in place afterwards (see
///     [`patch_linearized_deterministic_id`]). Either way the identifier is the
///     same width as the placeholder, so every later byte offset (`startxref`,
///     hint stream, xref offsets) is unchanged.
///   - `--static-id`: `[source_id0_or_π, π_const]`, with an empty source
///     `/ID[0]` falling back to the same pi value
///   - default: a fresh changing identifier; a non-empty source `/ID[0]` is
///     preserved, otherwise the same fresh value is used for both elements
///     (ISO 32000-1 §14.4).
fn finalize_linearized_id(
    options: &WriterOptions,
    source_id0: Option<&[u8]>,
    det_id_source_id0: Option<&[u8]>,
    copy_encryption: Option<&crate::encryption::CopyEncryptionSource>,
) -> Object {
    if options.deterministic_id {
        // Size the all-zero permanent-identifier placeholder to the source
        // `/ID[0]` length so the serialized `/ID` array reaches its FINAL width
        // here, before the two layout passes. qpdf preserves `/ID[0]` verbatim
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
    } else if let Some(source) = copy_encryption {
        let generated = crate::writer::generate_id_array_from_source_id0(None, options.static_id);
        let id1 = generated
            .as_array()
            .and_then(|values| values.get(1))
            .and_then(Object::as_string)
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| source.id0.clone());
        Object::Array(vec![
            Object::String(source.id0.clone()),
            Object::String(id1),
        ])
    } else {
        crate::writer::generate_id_array_from_source_id0(source_id0, options.static_id)
    }
}

/// Build qpdf's pass-1 `/ID` placeholder from the original trailer.
///
/// `QPDFWriter::writeTrailer` (qpdf 11.9.0, lines 1197-1213) ignores the
/// selected final-ID policy during linearization pass 1. It writes an all-zero
/// first string with the same byte width as the original `/ID[0]` (falling
/// back to 16 bytes when there is no non-empty original identifier), followed
/// by a 16-byte all-zero changing identifier.
fn linearization_pass1_id(source_id0: Option<&[u8]>) -> Object {
    let first_len = source_id0.map(|id| id.len()).unwrap_or(16);
    Object::Array(vec![
        Object::String(vec![0u8; first_len]),
        Object::String(vec![0u8; 16]),
    ])
}

/// `/ID` array for the split xref stream dicts.  Reads the file-scoped
/// identifier that `write_linearized` already finalized onto `source_trailer`
/// (see [`finalize_linearized_id`]) so it stays consistent with the Part-1
/// trailer.
/// Lift the writer-owned two-string `/ID` value into the canonical handle
/// graph. Linearization still keeps the legacy `Object` only for the existing
/// two-pass ID computation; all xref/trailer emission consumes this handle.
fn id_object_to_handle(object: &Object) -> Result<ObjectHandle> {
    let values = object.as_array().ok_or_else(|| {
        crate::Error::Unsupported("linearization writer: /ID is not an array".to_string())
    })?;
    if values.len() != 2 {
        return Err(crate::Error::Unsupported(
            "linearization writer: /ID must contain two strings".to_string(),
        ));
    }
    let ids = values
        .iter()
        .map(|value| {
            value
                .as_string()
                .map(|bytes| ObjectHandle::string(bytes.to_vec()))
                .ok_or_else(|| {
                    crate::Error::Unsupported(
                        "linearization writer: /ID entries must be strings".to_string(),
                    )
                })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ObjectHandle::array(ids))
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
/// `id0`/`id1` are precomputed by [`write_linearized_for_pdf_writer`] from a digest over a
/// reconstruction of qpdf's first write pass (the `det_id` computation; the
/// pass-1 buffer is built by [`build_pass1_part1`] with qpdf's `writePad`
/// length-stabilisation). That reconstruction is what reproduces qpdf's
/// deterministic `/ID` byte-for-byte. Here we only overwrite the all-zero
/// placeholders the final pass wrote at the xref-stream dict sites. Because the
/// replacement is the same width as the placeholder, no byte offset shifts.
///
/// The placeholder is replaced **only inside `id_ranges`** — the absolute byte
/// span of each emitted `/ID [<hex0><hex1>]` array *token itself*, reported by
/// [`xref_stream::write_object`] at the point it writes that token (see
/// [`patch_first_page_xref`] and `write_main_xref_stream_and_trailer`, the two
/// producers). Each range is exactly as wide as the placeholder
/// ([`crate::writer::deterministic_id_array_len`]), so at most one position in
/// it can ever match.
///
/// An earlier revision instead recorded the whole *section* containing a
/// `/ID` site (the full xref-stream object, including its stream payload and
/// — on the first-page xref stream — arbitrary custom (non-writer-owned)
/// trailer entries preserved verbatim from the source trailer). A custom
/// trailer value serialized as a PDF literal string does not escape `[`, `<`,
/// digits, `>`, or `]`, so a source document engineering such a value could
/// make that broader scan see a second, spurious match — tripping the
/// `debug_assert_eq!` below in debug builds and, in release builds,
/// corrupting that trailer entry's bytes. Tracking the exact token span
/// instead of the containing section removes that failure mode structurally:
/// a range this narrow has only one possible match position. Regression test:
/// `deterministic_id_objstm_survives_custom_trailer_placeholder_lookalike`.
///
/// The classic (stream-free) table path also pushes onto an `id_ranges`
/// vector inside the same `do_write_pass`, but with the old whole-section
/// span (`write_part1_xref_and_trailer` / `write_main_xref_and_trailer`).
/// That is harmless: `objstm_layout.is_empty()` picks one branch or the
/// other for the *entire* pass (both first-page and main-trailer sites), so
/// a classic-path run never produces the ObjStm-path pushes this function
/// consumes, and this function itself is only ever invoked when
/// `objstm_layout.is_empty()` is `false` (see the call site's guard). The
/// classic path's own `/ID` is direct-written via `id_writer` and never
/// reaches a placeholder at all, so it has no need of a precise span.
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
/// constant across both layout passes. A space placeholder of exactly that
/// length is written here;
/// [`patch_first_page_xref`] overwrites it with the real encoded object (qpdf
/// `/W [1 2 1]`, with `/Predictor 12` when filtered) plus trailing space padding once every
/// downstream offset (and the main xref offset for `/Prev`) is known — the region
/// length never changes, so no later byte shifts.
///
/// This stream is the **target of the file's trailing `startxref`** and holds
/// `/Prev → main xref` (qpdf's first-half → main chain direction); the main
/// (Part-6) xref at EOF carries no `/Prev`, so the chain is acyclic. Its
/// `/Index [second_half_count, first_half_count)` covers the FIRST-half objects.
//
// qpdf's `writeTrailer` emits `/Encrypt` immediately after `/ID` for every
// linearization trailer except `t_lin_second` (QPDFWriter.cc:1160-1231).
// `XrefStreamDict::encrypt` is an opt-in field because the same serializer is
// also used by the plain writer, whose xref-stream trailer has no such entry.
#[allow(clippy::too_many_arguments)]
fn write_first_page_xref_stream(
    bytes: &mut Vec<u8>,
    relocation: &ObjStmRelocation,
    total_count: u32, // /Size (relocated renumber.len() + 1) — already final
    catalog_new_ref: ObjectRef,
    info_new_ref: Option<ObjectRef>,
    source_trailer: &ObjectHandle,
    canonical_entries: &[(Vec<u8>, Vec<u8>)],
    max_ostream_index: u64,
    filtered: bool,
    encrypt: Option<ObjectRef>,
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
    let id = xref_id_bytes(source_trailer)?;
    let obj_ref = ObjectRef::new(first_xref_num, 0);

    // Reserve the fixed pass-1 region (qpdf's writePad length-stabilisation):
    // the forced wide field-2 (`1 << 25`) makes the region independent of the
    // hint length, so it is constant across both layout passes. The real
    // compressed object plus
    // trailing padding is written into the region by `patch_first_page_xref`
    // once every downstream offset (and the main xref offset for `/Prev`) is
    // known — so the region's byte length never changes and no later offset
    // shifts.
    let region_len = {
        let dict = xref_stream::XrefStreamDict {
            filtered,
            widths: xref_stream::first_pass_widths(max_id, max_ostream_index, 0),
            index: Some((index_start, index_count)),
            info: info_new_ref,
            root: Some(catalog_new_ref),
            size: final_size,
            prev: Some(0),
            trailer: None,
            canonical_entries: Some(canonical_entries),
            id: id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
            encrypt,
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
        canonical_entries: canonical_entries.to_vec(),
        encrypt,
        max_id,
        max_ostream_index,
        filtered,
    })
}

/// Extract the trailer `/ID`'s two byte strings — the deterministic-`/ID`
/// all-zero placeholder while writing — for the rebuilt xref-stream dicts. The
/// real identifier is patched in afterwards by [`patch_linearized_deterministic_id`].
fn xref_id_bytes(source_trailer: &ObjectHandle) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let id = source_trailer.try_get_key(b"/ID")?;
    let Some(values) = id.try_as_array()? else {
        return Ok(None);
    };
    if values.len() != 2 {
        return Ok(None);
    }
    let (first, second) = (&values[0], &values[1]);
    Ok(match (first.as_string(), second.as_string()) {
        (Some(first), Some(second)) => Some((first, second)),
        _ => None,
    })
}

/// Overwrite the first-page xref stream's reserved region with the real encoded
/// object once every downstream offset (and the main xref offset for `/Prev`)
/// is known.
///
/// The object is rebuilt from scratch — entries from `xref_offsets` /
/// `member_new`, qpdf-matching `/W` widths, and policy-selected raw or
/// PNG-`/Predictor 12` + Flate payload,
/// `/Prev → main xref` — then space-padded to the region's fixed byte length
/// ([`xref_stream::write_padded_region`]). Because the region length is fixed
/// (qpdf's pass-1 sizing), this shifts no later offset while the hint object is
/// spliced. The rebuilt dict carries the all-zero `/ID`
/// placeholder, which [`patch_linearized_deterministic_id`] overwrites later.
///
/// Returns the absolute byte range (within `bytes`) of the emitted `/ID`
/// array token, translated from [`xref_stream::write_padded_region`]'s
/// region-relative range by `patch.region.start`. The caller records this
/// exact span in `id_ranges` instead of the whole region — the region also
/// carries this object's stream payload and, via `patch.canonical_entries`,
/// arbitrary custom trailer entries whose serialized bytes could otherwise
/// coincidentally match the placeholder's fixed byte pattern (see
/// [`patch_linearized_deterministic_id`]'s doc).
fn patch_first_page_xref(
    bytes: &mut [u8],
    patch: &FirstPageXrefPatch,
    xref_offsets: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
    main_xref_offset: usize,
    hint_length: usize,
    pass1: bool,
) -> Result<Option<std::ops::Range<usize>>> {
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
        let payload = if patch.filtered {
            xref_stream::encode_payload_uncompressed(&entries, widths)?
        } else {
            xref_stream::encode_payload_raw(&entries, widths)?
        };
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
        let payload = if patch.filtered {
            xref_stream::encode_payload(&entries, widths)?
        } else {
            xref_stream::encode_payload_raw(&entries, widths)?
        };
        (widths, payload, main_xref_offset as u64)
    };
    let dict = xref_stream::XrefStreamDict {
        filtered: patch.filtered,
        widths,
        index: Some((patch.index_start, patch.index_count)),
        info: patch.info_new_ref,
        root: Some(patch.catalog_new_ref),
        size: patch.size,
        prev: Some(prev),
        trailer: None,
        canonical_entries: Some(&patch.canonical_entries),
        id: patch.id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
        encrypt: patch.encrypt,
    };
    // cov:ignore: the `?` below never fires — write_padded_region errors only if
    // the object exceeds its pass-1-sized region. Filtered final payloads fit
    // inside the wider predicted region; raw payloads retain the same size.
    let (region, region_id_range) = xref_stream::write_padded_region(
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
    Ok(region_id_range.map(|r| patch.region.start + r.start..patch.region.start + r.end))
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
/// `(main_xref_offset, main_xref_offset, id_range)`: the caller computes `/T =
/// main_xref_offset − 1` (via `saturating_sub(1)`), matching qpdf's
/// `xref_zero_offset` (the byte just before the main xref stream object).
/// `id_range` is the absolute byte range (within `bytes`) of the emitted
/// `/ID` array token — see [`patch_first_page_xref`]'s doc for why the caller
/// records this exact span rather than the whole object.
#[allow(clippy::too_many_arguments)]
fn write_main_xref_stream_and_trailer(
    bytes: &mut Vec<u8>,
    xref_offsets: &BTreeMap<u32, usize>,
    member_new: &BTreeMap<u32, (u32, u32)>,
    relocation: &ObjStmRelocation,
    total_count: u32, // /Size (placed renumber.len() + 1) — already final
    source_trailer: &ObjectHandle,
    first_page_obj_offset: usize,
    max_ostream_index: u64,
    pass1: bool,
    filtered: bool,
) -> Result<(usize, usize, Option<std::ops::Range<usize>>)> {
    let final_size = total_count;
    let first_xref_num = relocation.first_xref_slot;
    let main_xref_num = relocation.main_xref_slot;
    let max_id = final_size.saturating_sub(1);
    let id = xref_id_bytes(source_trailer)?;

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
    // With compression enabled, pass 1 writes the uncompressed PNG-predicted
    // payload (qpdf's `skip_compression`) and the final pass Flate-compresses
    // it. With compression disabled, both passes write raw `/W` rows.
    let payload = if !filtered {
        xref_stream::encode_payload_raw(&entries, widths)?
    } else if pass1 {
        xref_stream::encode_payload_uncompressed(&entries, widths)?
    } else {
        xref_stream::encode_payload(&entries, widths)?
    };

    // Main (second-half) xref: no `/Index`, `/Info`, `/Root`, or `/Prev` — it is
    // the chain terminal, reached only via the first-page stream's `/Prev`. Its
    // `/Size` is the second-half object COUNT (not the file's total /Size), so
    // the omitted `/Index` defaults to `[0, main_count)` — exactly the objects
    // this stream covers (qpdf's `second_trailer_size`).
    let dict = xref_stream::XrefStreamDict {
        filtered,
        widths,
        index: None,
        info: None,
        root: None,
        size: main_count,
        prev: None,
        trailer: None,
        canonical_entries: None,
        id: id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
        encrypt: None,
    };

    // Pad the object to its fixed pass-1 region (qpdf's writePad), then a newline
    // before `startxref`, so the file length is independent of the compressed
    // length. Unlike the first-page stream, the main xref's pass-1 `max_offset`
    // is its OWN offset (`second_xref_offset`) — already known — so qpdf does NOT
    // force the wide 4-byte field here; the region uses the real `/W` widths.
    let main_obj_ref = ObjectRef::new(main_xref_num, 0);
    let region_len = {
        let p1_dict = xref_stream::XrefStreamDict {
            filtered,
            widths,
            index: None,
            info: None,
            root: None,
            size: main_count,
            prev: None,
            trailer: None,
            canonical_entries: None,
            id: id.as_ref().map(|(a, b)| (a.as_slice(), b.as_slice())),
            encrypt: None,
        };
        xref_stream::first_pass_region_len(main_obj_ref, &p1_dict, main_count as usize)
    };
    let (region, region_id_range) =
        xref_stream::write_padded_region(main_obj_ref, &dict, &payload, region_len)?;
    let region_start = bytes.len();
    bytes.extend_from_slice(&region);
    bytes.push(b'\n');
    let id_range = region_id_range.map(|r| region_start + r.start..region_start + r.end);

    // Trailing `startxref` → the **first-page** xref stream (qpdf's chain leaf).
    bytes.extend_from_slice(format!("startxref\n{first_page_obj_offset}\n%%EOF\n").as_bytes());

    // `/T` rule for the split linearized file is the byte just before the
    // **main** cross-reference stream (qpdf's `xref_zero_offset`). The caller
    // computes `/T = second_return.saturating_sub(1)`, so return
    // `main_xref_offset` as the second element. The first element is also the
    // main xref offset (used for layout diagnostics / `last_xref`).
    Ok((main_xref_offset, main_xref_offset, id_range))
}

/// Serialize the hint-stream object dictionary + `stream\n` opener exactly as
/// qpdf 11.9.0 orders it: an optional `/Filter /FlateDecode`, followed by
/// `/S {s}[ /O {o}] /Length {len}`.
/// qpdf emits `/O` between `/S` and `/Length`, and only when the document has
/// outlines (`if (O)`, QPDFWriter.cc:2307); `/S` carries the shared-object
/// section offset, `/O` the outlines section offset (both within the
/// uncompressed hint stream).
///
/// Used by [`append_hint_stream_object`] so the hand-written dictionary keeps
/// qpdf's key order. Stream framing is emitted by the canonical writer
/// pipeline immediately after this prefix.
fn hint_stream_dict_prefix(
    shared_section_offset: usize,
    outline_section_offset: Option<usize>,
    payload_len: usize,
    filtered: bool,
) -> String {
    let filter_key = if filtered {
        "/Filter /FlateDecode "
    } else {
        ""
    };
    let outline_key = match outline_section_offset {
        Some(o) => format!(" /O {o}"),
        None => String::new(),
    };
    format!("<< {filter_key}/S {shared_section_offset}{outline_key} /Length {payload_len} >>")
}

#[cfg(test)]
thread_local! {
    /// Test-only IV queue used to model a probe/final re-encryption regression
    /// without exposing an IV-injection option in the production API. The
    /// queue is thread-local because Rust unit tests run in parallel.
    static TEST_HINT_STREAM_AES_IVS:
        std::cell::RefCell<std::collections::VecDeque<[u8; 16]>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

#[cfg(test)]
fn next_test_hint_stream_aes_iv(default: [u8; 16]) -> [u8; 16] {
    TEST_HINT_STREAM_AES_IVS.with(|ivs| ivs.borrow_mut().pop_front().unwrap_or(default))
}

#[cfg(test)]
fn with_test_hint_stream_aes_ivs<T>(
    ivs: impl IntoIterator<Item = [u8; 16]>,
    f: impl FnOnce() -> T,
) -> (T, Vec<[u8; 16]>) {
    let previous = TEST_HINT_STREAM_AES_IVS.with(|slot| slot.replace(ivs.into_iter().collect()));
    let result = f();
    let remaining = TEST_HINT_STREAM_AES_IVS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let remaining = slot.drain(..).collect();
        *slot = previous;
        remaining
    });
    (result, remaining)
}

/// Emit the primary hint-stream object and return its start byte offset.
///
/// qpdf 11.9.0 serializes the hint-stream object dict in the key order
/// optional `/Filter`, then `/S`, `/O` when present, and `/Length` (observed
/// against its `--check-linearization` golden output), which the generic
/// `BTreeMap`-ordered [`Object::Stream`] serializer cannot reproduce. This
/// emitter writes the dict literal by hand (via [`hint_stream_dict_prefix`]) to
/// match that order; the surrounding framing (`N G obj\n` … `\nstream\n` …
/// `\nendstream\nendobj\n`) is byte-identical to [`append_object`]. The newline
/// before `endstream` is written only when the payload does not already end in
/// one (qpdf, QPDFWriter.cc:2327).
///
/// The hint stream IS encrypted when `encrypt_ctx` is `Some` — unlike the
/// `/Encrypt` dict and the xref table/stream, it carries no exemption in
/// qpdf: `writeHintStream` calls `setDataKey(hint_id)` before writing the
/// dict/payload (QPDFWriter.cc:2297), so the emitted `/Length` reflects the
/// *encrypted* byte count. `new_ref` (the hint stream's own reserved object
/// number) is always distinct from `ctx.encrypt_ref` — the `/Encrypt` dict's
/// slot is reserved by inserting immediately before the (then-current) hint
/// slot and shifting the latter by one (`RenumberMap::reserve_encrypt_dict_slot`)
/// — so no self-skip check is needed here, unlike [`append_object`] and
/// [`append_body_object`].
///
/// `hint_stream_aes_iv` is used only while constructing this one complete
/// buffer. qpdf encrypts the hint stream once and replays the exact framed
/// bytes on its second layout pass (`QPDFWriter.cc:2860-2884`); the caller
/// follows the same boundary by passing the resulting buffer to
/// [`do_write_pass`] unchanged. It is unused when `encrypt_ctx` is `None` or
/// the cipher is RC4 (no IV concept).
#[allow(clippy::too_many_arguments)]
fn append_hint_stream_object(
    bytes: &mut Vec<u8>,
    new_ref: ObjectRef,
    payload: &[u8],
    shared_section_offset: usize,
    outline_section_offset: Option<usize>,
    filtered: bool,
    encrypt_ctx: Option<&crate::writer::EncryptionContext>,
    hint_stream_aes_iv: [u8; 16],
) -> Result<usize> {
    #[cfg(test)]
    let hint_stream_aes_iv = next_test_hint_stream_aes_iv(hint_stream_aes_iv);

    // qpdf calls `adjustAESStreamLength` after selecting the hint object's data
    // key and before writing its dictionary (`QPDFWriter.cc:2296-2314`). The
    // canonical writer pipeline emits the IV and ciphertext later, but this
    // fixed-width length is needed now for the hand-ordered dictionary.
    let mut payload_len = payload.len();
    if let Some(ctx) = encrypt_ctx {
        crate::writer::adjust_aes_stream_length(&mut payload_len, ctx, true)?;
    }

    let offset = bytes.len();
    bytes.extend_from_slice(format!("{} {} obj\n", new_ref.number, new_ref.generation).as_bytes());
    bytes.extend_from_slice(
        hint_stream_dict_prefix(
            shared_section_offset,
            outline_section_offset,
            payload_len,
            filtered,
        )
        .as_bytes(),
    );
    if let Some(ctx) = encrypt_ctx {
        // qpdf's `writeHintStream` selects the hint object's data key and
        // writes the payload through the encryption pipeline exactly once.
        // Pass 2 receives this complete framed object unchanged, so the
        // explicit IV preserves the same ciphertext across both passes.
        crate::writer::write_stream_payload_with_pipeline_qdf(
            bytes,
            payload,
            NewlineBeforeEndstream::Never,
            true,
            new_ref,
            ctx,
            true,
            Some(hint_stream_aes_iv),
        )?;
    } else {
        crate::writer::serialize::write_stream_payload_with_qdf(
            bytes,
            payload,
            NewlineBeforeEndstream::Never,
            true,
        );
    }
    bytes.extend_from_slice(b"\nendobj\n");
    Ok(offset)
}

/// Loop-invariant inputs for the Outlines Hint Table (qpdf's `c_outline_data`).
///
/// `first_object` and `nobjects` depend only on membership + renumbering; the
/// pass-1 offset/length are filled in to build the [`OutlineHintTable`].
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
/// Propagates reader errors from catalog resolution.
fn compute_outline_hint_info<R: Read + Seek>(
    outlines: &std::collections::BTreeSet<ObjectRef>,
    pdf: &mut Pdf<R>,
    renumber: &RenumberMap,
    objstm_layout: &ObjStmLayout,
) -> Result<Option<OutlineHintInfo>> {
    if outlines.is_empty() {
        return Ok(None);
    }
    // The /Outlines dictionary reference (the first outline unit qpdf places).
    // This helper runs only when the retained outline set is non-empty
    // (therefore a /Outlines key exists),
    // so the catalog is always a resolvable dictionary here.
    let outlines_ref = pdf.root_ref().and_then(|r| match pdf.resolve_borrowed(r) {
        Ok(Object::Dictionary(d)) => d.get_ref("Outlines"),
        _ => None, // cov:ignore: catalog is always a dict when outlines exist
    });
    let Some(outlines_ref) = outlines_ref else {
        // Defensive: a non-empty outline closure implies a /Outlines ref, so this
        // is unreachable for a well-formed catalog.
        return Ok(None); // cov:ignore: retained outline set non-empty implies catalog /Outlines ref present
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

/// Build the Outlines Hint Table from qpdf's pass-1 measurements
/// (`calculateHOutline`).
///
/// `first_object_offset` is already the pass-1 virtual offset: pass 1 omits
/// the reserved hint object, so qpdf's final `adjusted_offset` adds the exact
/// saved hint-object length back when validating pass 2. `group_length` is the
/// summed byte length of the `nobjects` consecutive output units starting at
/// `first_object` (qpdf's `outputLengthNextN`).
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if the first outline unit has no
/// offset in `xref_offsets` or if the virtual offset exceeds the fixed
/// 32-bit hint-table field.
fn build_outline_hint_table(
    info: &OutlineHintInfo,
    xref_offsets: &BTreeMap<u32, usize>,
    byte_lengths: &BTreeMap<u32, usize>,
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
    // The HGeneric `first_object_offset` is a fixed 32-bit field (qpdf
    // writeHGeneric). Reject (rather than silently truncate) an offset past
    // 4 GiB, matching how the other fixed-width hint fields fail via
    // `write_bits_checked`.
    let first_object_offset = u32::try_from(first_off).map_err(|_| {
        crate::Error::Unsupported(format!(
            "outline hint: pass-1 first unit offset ({first_off}) exceeds the \
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
/// finalized Part-1 bytes and overwriting the rewritable dict region (`<<`
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

/// Result of one complete linearized layout pass.
///
/// Pass 1 omits the reserved hint object, so its offsets are qpdf's virtual
/// coordinates. The final pass contains the exact hint-object buffer generated
/// from this result.
struct LinearizedPassOutput {
    bytes: Vec<u8>,
    xref_offsets: BTreeMap<u32, usize>,
    first_page_xref_offset: Option<usize>,
    hint_stream_offset: usize,
    hint_stream_obj_total_len: usize,
    end_of_first_page_offset: usize,
    last_xref_offset: usize,
    last_xref_first_entry_offset: usize,
    first_trailer_prev_range: std::ops::Range<usize>,
    id_ranges: Vec<std::ops::Range<usize>>,
}

/// Perform a complete single-pass write of the linearized PDF body.
///
/// `hint_stream_object` is the complete qpdf-shaped indirect hint object. `None`
/// is qpdf pass 1: the reserved slot is omitted and all objects after it use
/// virtual offsets. `Some` splices the already-encrypted/framed object without
/// re-encoding it. `structural_streams_filtered` controls ObjStm and xref
/// stream filters and payload encodings.
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
/// classic body uses the flag for its zeroed first-page xref, while the ObjStm
/// path uses it for qpdf's pass-1 xref-stream representation.
///
/// `encrypt_ctx`, when `Some`, is emitted as a plaintext `/Encrypt` indirect
/// object right after the catalog/open-document-plain objects, in every pass
/// (qpdf writes it unconditionally on `m->encrypted`, independent of pass
/// number — see the call site's doc for the qpdf source reference).
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
    hint_stream_object: Option<&[u8]>,
    structural_streams_filtered: bool,
    source_trailer: &ObjectHandle,
    objstm_layout: &ObjStmLayout,
    relocation: &ObjStmRelocation,
    options: &WriterOptions,
    pass1_digest: bool,
    mut id_writer: Option<crate::object::ReborrowableIdWriter>,
    encrypt_ctx: Option<&crate::writer::EncryptionContext>,
    mut encrypted_string_emitter: Option<&mut EncryptedStringEmitter>,
) -> Result<LinearizedPassOutput> {
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
    // qpdf deliberately writes extra header text after the linearization
    // parameter dictionary, rather than in `writeHeader`, so the dictionary
    // remains within the first 1024 bytes (QPDFWriter.cc:2718-2720). The
    // setting has already been normalized with a trailing newline by
    // PdfWriter, matching qpdf's `setExtraHeaderText` contract.
    bytes.extend_from_slice(options.extra_header_text.as_bytes());
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

    let trailer_map = |object_ref| {
        // cov:ignore-start: planner reachability and renumber placement cover every live trailer reference.
        renumber.new_for_original(object_ref).ok_or_else(|| {
            crate::Error::Unsupported(format!(
                "linearization writer: trailer reference {object_ref} has no renumber entry"
            ))
        })
        // cov:ignore-end
    }; // cov:ignore: planner-produced trailer map is complete by construction.
    let canonical_entries =
        canonical_linearization_trailer_entries(source_trailer, &trailer_map, &plan.removed_refs)?;

    // The classic Part-1 mini-xref + first trailer is only emitted on the
    // non-ObjStm path.  For ObjStm-bearing output the first-page (Part-1)
    // *xref stream* takes its place — and, per the flpdf-56u review, it MUST
    // sit physically here (inside the first-page region, before /E) so a
    // reader can resolve page 1 from the leading bytes.  It is written with a
    // deterministic byte length (uncompressed payload + fixed-width /Prev) and
    // back-patched in place once the downstream offsets and the main (Part-6)
    // xref offset are known (see `patch_first_page_xref` below) — so this shifts
    // no bytes between the two layout passes. Returning an empty
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
            &canonical_entries,
            &trailer_map,
            &plan.removed_refs,
            id_writer.as_deref_mut(),
            encrypt_ctx,
        )?; // cov:ignore: the validated linearization plan makes this serializer error path defensive.
        part1_classic_xref_offset = p1_xref_offset;
        part1_xref_patch = Some(patch);
        // Part-1 first-page trailer `/ID` site.  The main (Part-6) trailer
        // emitted at EOF carries the same `/ID` (its span is captured at
        // the `write_main_xref_and_trailer` call below), so the classic
        // table path has two `/ID` sites — both back-patched together.
        id_ranges.push(section_start..bytes.len());
        range
    } else {
        let patch = write_first_page_xref_stream(
            &mut bytes,
            relocation,
            total_count,
            catalog_new_ref,
            info_new_ref,
            source_trailer,
            &canonical_entries,
            max_ostream_index,
            structural_streams_filtered,
            encrypt_ctx.map(|ctx| ctx.encrypt_ref),
        )?;
        // First-page xref stream object carries one `/ID` (the main xref
        // stream below carries the second). This call only reserves the
        // region as blank padding — the object's real bytes, including its
        // `/ID` token and canonical (custom) trailer entries, are written
        // later by `patch_first_page_xref`, which is the one that reports the
        // token's exact span for `id_ranges` (see that function's doc for why
        // the whole region is not used: it also carries this object's stream
        // payload and, via `canonical_entries`, arbitrary custom trailer
        // entries whose serialized bytes could coincidentally contain the
        // deterministic-`/ID` placeholder's fixed byte pattern).
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
        let offset = append_body_object_for_ref(
            &mut bytes,
            pdf,
            catalog_new_ref,
            catalog_orig,
            options,
            encrypt_ctx,
            encrypted_string_emitter.as_deref_mut(),
            renumber,
            &plan.removed_refs,
            &plan.content_normalize_refs,
        )?; // cov:ignore: planner-produced Catalog references are valid by construction.
        xref_offsets.insert(catalog_new_ref.number, offset);
        report_progress_event(options)?;
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
        if catalog_emitted_early && plan.root_ref == Some(*original_ref) {
            continue;
        }
        // cov:ignore-start: unreachable invariant — renumber.rs step-6b inserts
        // every part4_open_document_plain ref, so new_for_original is always Some.
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(
                "part4_open_document_plain ref missing from renumber map".into(),
            ));
        };
        // cov:ignore-end
        let offset = append_body_object_for_ref(
            &mut bytes,
            pdf,
            new_ref,
            *original_ref,
            options,
            encrypt_ctx,
            encrypted_string_emitter.as_deref_mut(),
            renumber,
            &plan.removed_refs,
            &plan.content_normalize_refs,
        )?; // cov:ignore: planner-produced open-document references are valid by construction.
        xref_offsets.insert(new_ref.number, offset);
        report_progress_event(options)?;
    }

    // Open-document ObjStm containers (qpdf part4).  qpdf places the
    // open-document objects (`/OpenAction`, `/AcroForm`, … subtrees) in part4,
    // physically right after the Catalog and BEFORE the primary hint stream —
    // their object numbers (`part4_first_obj …`) sit between the catalog and the
    // hint id (QPDFWriter.cc:2606-2612).  The container itself is a plain
    // indirect object; its compressed members are emitted nowhere else (skipped
    // in every plain loop via `member_to_container`).
    for container in &objstm_layout.open_document {
        let offset = append_objstm_container_object(
            &mut bytes,
            container,
            renumber,
            pdf,
            &plan.removed_refs,
            structural_streams_filtered,
            encrypt_ctx,
        )?; // cov:ignore: error requires an internal planner/renumber inconsistency.
        xref_offsets.insert(container.container_new_num, offset);
        for _ in &container.members {
            if pass1_digest {
                decrement_progress_event(options)?;
            }
            report_progress_event(options)?;
            if pass1_digest {
                // qpdf's writeObjectStream performs an offset-measuring pass
                // followed by the payload pass inside each outer linearization
                // pass. The first outer pass therefore reports each member
                // twice after the decrement, while the final pass's net effect
                // is one event (QPDFWriter.cc:1639-1707).
                report_progress_event(options)?;
            }
        }
    }

    // `/Encrypt` dictionary object (qpdf `writeEncryptionDictionary`, called
    // from `writeLinearized` right after `part4_end_marker` —
    // QPDFWriter.cc:2793-2796). Emitted right after the Part-4
    // (open-document) objects — plain and ObjStm-container — and before the
    // hint stream, matching qpdf's insertion point. qpdf calls
    // `writeEncryptionDictionary` unconditionally on `m->encrypted`, on
    // every pass, so this is not gated on `pass1_digest`. The dict itself is
    // never encrypted (PDF 1.7 §7.6.1 — a reader must parse it before it can
    // derive the file key needed to decrypt anything else). Its five binary
    // security-handler strings use the dedicated compact hexadecimal form.
    if let Some(ctx) = encrypt_ctx {
        let offset = bytes.len();
        bytes.extend_from_slice(
            format!(
                "{} {} obj\n",
                ctx.encrypt_ref.number, ctx.encrypt_ref.generation
            )
            .as_bytes(),
        );
        crate::writer::encrypted_strings::write_encryption_dictionary(
            &mut bytes,
            &ctx.encrypt_dict,
        );
        bytes.extend_from_slice(b"\nendobj\n");
        xref_offsets.insert(ctx.encrypt_ref.number, offset);
    }

    // Hint stream object.
    //
    // In pass-1-digest mode the hint stream is absent (qpdf reserves its xref
    // slot but writes no bytes during pass 1), so every object physically after
    // it shifts down by the hint length.  Skipping the emission here reproduces
    // that shift incrementally — no offset arithmetic.  The slot is also kept
    // out of `xref_offsets`: the first-page xref that covers it is written as
    // formatted zero-offset entries below, so the slot needs no real offset.
    let hint_stream_offset = bytes.len();
    if let Some(hint_stream_object) = hint_stream_object {
        bytes.extend_from_slice(hint_stream_object);
        xref_offsets.insert(hint_stream_new_num, hint_stream_offset);
    }
    let hint_stream_obj_total_len = bytes.len() - hint_stream_offset;

    // qpdf orders every first-page plain object and Part-3 ObjStm container by
    // the object number assigned during its linearization setup. In particular,
    // an optimization-minted inherited attribute can follow a container that
    // was allocated before optimization. Build one emission list so physical
    // order agrees with RenumberMap and the hint table.
    enum FirstPageEmit<'a> {
        Plain(ObjectRef),
        Container(&'a ObjStmContainer),
    }
    let mut first_page_emits: Vec<(u32, FirstPageEmit<'_>)> = Vec::new();
    for original_ref in plan
        .part2_objects
        .iter()
        .chain(&plan.part3_objects)
        .chain(&plan.part6_outline_objects)
    {
        if objstm_layout.member_to_container.contains_key(original_ref)
            || (catalog_emitted_early && plan.root_ref == Some(*original_ref))
        {
            continue;
        }
        // cov:ignore-start: planner/renumber inconsistency -- impossible by construction, since part2/part3/part6_outline_objects derive from the same renumber plan.
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "first-page object {} has no renumber entry",
                original_ref
            )));
        };
        // cov:ignore-end
        first_page_emits.push((new_ref.number, FirstPageEmit::Plain(*original_ref)));
    }
    for container in &objstm_layout.part3 {
        first_page_emits.push((
            container.container_new_num,
            FirstPageEmit::Container(container),
        ));
    }
    first_page_emits.sort_by_key(|(number, _)| *number);

    for (_, emit) in first_page_emits {
        match emit {
            FirstPageEmit::Plain(original_ref) => {
                let new_ref = renumber
                    .new_for_original(original_ref)
                    .expect("first-page plain object renumber entry checked above");
                let offset = append_body_object_for_ref(
                    &mut bytes,
                    pdf,
                    new_ref,
                    original_ref,
                    options,
                    encrypt_ctx,
                    encrypted_string_emitter.as_deref_mut(),
                    renumber,
                    &plan.removed_refs,
                    &plan.content_normalize_refs,
                )?; // cov:ignore: planner-produced first-page references are valid by construction.
                xref_offsets.insert(new_ref.number, offset);
                report_progress_event(options)?;
            }
            FirstPageEmit::Container(container) => {
                let offset = append_objstm_container_object(
                    &mut bytes,
                    container,
                    renumber,
                    pdf,
                    &plan.removed_refs,
                    structural_streams_filtered,
                    encrypt_ctx,
                )?; // cov:ignore: error requires an internal planner/renumber inconsistency.
                xref_offsets.insert(container.container_new_num, offset);
                for _ in &container.members {
                    if pass1_digest {
                        decrement_progress_event(options)?;
                    }
                    report_progress_event(options)?;
                    if pass1_digest {
                        report_progress_event(options)?;
                    }
                }
            }
        }
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
                let offset = append_body_object_for_ref(
                    &mut bytes,
                    pdf,
                    new_ref,
                    *original_ref,
                    options,
                    encrypt_ctx,
                    encrypted_string_emitter.as_deref_mut(),
                    renumber,
                    &plan.removed_refs,
                    &plan.content_normalize_refs,
                )?; // cov:ignore: planner-produced Part-4 references are valid by construction.
                xref_offsets.insert(new_ref.number, offset);
                report_progress_event(options)?;
            }
            Part4Emit::Container(container) => {
                let offset = append_objstm_container_object(
                    &mut bytes,
                    container,
                    renumber,
                    pdf,
                    &plan.removed_refs,
                    structural_streams_filtered,
                    encrypt_ctx,
                )?; // cov:ignore: error requires an internal planner/renumber inconsistency.
                xref_offsets.insert(container.container_new_num, offset);
                for _ in &container.members {
                    if pass1_digest {
                        decrement_progress_event(options)?;
                    }
                    report_progress_event(options)?;
                    if pass1_digest {
                        report_progress_event(options)?;
                    }
                }
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
            &trailer_map,
            &plan.removed_refs,
            // Last use of `id_writer` — move it (no reborrow needed).
            id_writer,
        )?; // cov:ignore: the validated linearization plan makes this serializer error path defensive.
        id_ranges.push(main_section_start..bytes.len());

        // Every first-page object offset is now known, so back-patch the
        // Part-1 first-page xref's placeholder entry block in place. The block
        // length was reserved exactly, so this shifts no bytes between passes.
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
            structural_streams_filtered,
        )?; // cov:ignore: the validated linearization plan makes xref-stream serialization errors defensive.
        let (main_xref_offset, main_first_entry_offset, main_id_range) = result;
        // Main xref stream object is the second (and last) `/ID` site on the
        // ObjStm path.  Its span extends through the trailing
        // `startxref`/`%%EOF` and is never touched by `patch_first_page_xref`
        // below (which patches only the first-page region, before /E). Record
        // only the `/ID` array token's own span (not the whole object): this
        // dict carries no canonical (custom) trailer entries
        // (`write_main_xref_stream_and_trailer` always passes
        // `canonical_entries: None`), but its stream payload is arbitrary
        // binary xref data, so the tight span still avoids scanning bytes
        // this function does not own. See `patch_first_page_xref`'s doc for
        // the full rationale (that first-page sibling *does* carry custom
        // trailer entries, which is what makes the tight span load-bearing
        // there).
        if let Some(id_range) = main_id_range {
            id_ranges.push(id_range);
        }

        // Every downstream object offset is now known, so rebuild the first-page
        // xref's reserved region with the real encoded object and `/Prev →
        // main xref`. The region's byte length is fixed (qpdf's pass-1 sizing),
        // so this shifts no bytes between the two layout passes.
        let first_page_id_range = patch_first_page_xref(
            &mut bytes,
            patch,
            &xref_offsets,
            &member_new,
            main_xref_offset,
            hint_stream_obj_total_len,
            pass1_digest,
        )?; // cov:ignore: propagates patch_first_page_xref's unreachable region-overflow error arm.
        if let Some(id_range) = first_page_id_range {
            id_ranges.push(id_range);
        }

        (main_xref_offset, main_first_entry_offset)
    };

    Ok(LinearizedPassOutput {
        bytes,
        xref_offsets,
        first_page_xref_offset: first_page_xref_patch
            .as_ref()
            .map(|patch| patch.region.start),
        hint_stream_offset,
        hint_stream_obj_total_len,
        end_of_first_page_offset,
        last_xref_offset,
        last_xref_first_entry_offset,
        first_trailer_prev_range,
        id_ranges,
    })
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

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

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
                // cov:ignore-start: first-half routes cannot enter resolved second-half batches
                ContainerPart::OpenDocument
                | ContainerPart::FirstPagePrivate
                | ContainerPart::FirstPageShared
                | ContainerPart::FirstPageOutlines => {
                    unreachable!("first-half route in second-half ObjStm batches")
                } // cov:ignore-end
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

/// Reject a standard (non-ObjStm-generating) linearization plan that still
/// contains multiple live objects sharing an object number with different
/// generations.
///
/// qpdf's own linearization renumbering pass hits this exact limitation for
/// files with a stale-generation reference alongside the object's real
/// generation (e.g. a `/Candidates [4 0 R 4 1 R]`-shaped array referencing
/// both a dangling `4 0 R` and the live `4 1 R`): `discard_lower_generations`
/// only removes duplicate rows from the *raw xref table* at load time, so it
/// does not resolve a reference to a generation that never had its own xref
/// row in the first place. Verified against a live qpdf 11.9.0 probe: both
/// `--object-streams=disable` and `--object-streams=preserve` reject
/// `null-visible-stale-generation.pdf` with this exact message (exit 2),
/// while `--object-streams=generate` succeeds because Generate's own
/// planning already collapses stale generations before this point.
fn reject_multiple_generations(plan: &LinearizationPlan) -> Result<()> {
    let mut previous_number = None;
    for object_ref in plan.renumber_assigned_refs() {
        if previous_number == Some(object_ref.number) {
            return Err(crate::Error::Unsupported(
                "QPDF cannot currently linearize files that contain multiple objects with the \
                 same object ID and different generations.  If you see this error message, \
                 please file a bug report and attach the file if possible.  As a workaround, \
                 first convert the file with qpdf without linearizing, and then linearize the \
                 result of that conversion."
                    .to_string(),
            ));
        }
        previous_number = Some(object_ref.number);
    }
    Ok(())
}

/// Resolved state of the destination Catalog's `/Extensions /ADBE` entry —
/// gathered in a single Catalog/`/Extensions` resolve pass.
struct CatalogAdbeStatus {
    /// Whether an `/ADBE` key exists under `/Extensions`, in ANY form —
    /// Dictionary or Reference. Matches qpdf's key-existence-based removal
    /// trigger (QPDFWriter.cc L1387: `keys.count("/ADBE") > 0`), not
    /// `/ExtensionLevel` validity.
    has_adbe: bool,
    /// Whether [`inject_adbe_extension`]/[`strip_adbe_extension`] mutating
    /// the Catalog in place would silently drop an indirect reference.
    ///
    /// The underlying invariant is shape-independent: both helpers replace
    /// `/Extensions` (when it is itself indirect) or its `/ADBE` entry
    /// (`extensions.insert("ADBE", ..)` / `extensions.remove("ADBE")`)
    /// *wholesale*, never incrementally patching a value that was already
    /// there. So **any** indirect reference reachable anywhere within the
    /// ORIGINAL `/Extensions` subtree — the `/Extensions` value itself, or
    /// any dictionary value / array element nested within it at any depth
    /// (e.g. a direct `/ADBE` dict whose own `/ExtensionLevel` is an
    /// indirect reference, not just `/ADBE` itself) — loses its only edge
    /// once the enclosing value is replaced. This field is `true` whenever
    /// [`collect_direct_refs`] finds at least one [`Object::Reference`]
    /// anywhere in that subtree; no case-by-case shape enumeration is
    /// needed or attempted.
    ///
    /// `crate::writer::emit_canonical_pdf_inner` can absorb any such
    /// case safely: it mutates the Catalog and THEN builds its
    /// `CatalogFirstRenumber` from the SAME (now-mutated) handle
    /// (`writer.rs:3154-3238`), so a dropped object simply never gets a
    /// slot. [`write_linearized_for_pdf_writer`] cannot: its `plan`/`renumber` are built
    /// by the CALLER from a SEPARATE `Pdf` handle BEFORE this function ever
    /// runs (see the doc above the `Optimization::prepare_for_linearized_write`
    /// call below), so an object dropped this way is already counted in
    /// that frozen `plan` and would still be walked and emitted — with its
    /// STALE, now-orphaned, pre-mutation content — a genuine byte
    /// divergence from qpdf, not a cosmetic one. [`write_linearized_for_pdf_writer`]
    /// checks this so any such case is rejected loudly (`Unsupported`)
    /// instead of silently producing wrong bytes.
    orphans_indirect_object: bool,
}

/// Resolve [`CatalogAdbeStatus`] for `pdf`'s destination Catalog.
fn resolve_catalog_adbe_status<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<CatalogAdbeStatus> {
    const NONE: CatalogAdbeStatus = CatalogAdbeStatus {
        has_adbe: false,
        orphans_indirect_object: false,
    };

    // cov:ignore-start: defensive /Root guard. `write_linearized`'s only
    // caller context reaches this point with a `plan` whose `root_ref` is
    // `Some` (`plan.root_ref.ok_or_else(Unsupported)` below, on the SAME
    // source bytes as `pdf`), so `pdf.root_ref()` is `None` here only for a
    // caller that deliberately passes a `plan`/`pdf` pair built from
    // different sources — not exercised by any fixture in this test module.
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(NONE);
    };
    // cov:ignore-end

    // Resolve the Catalog once. From within that single borrow: (a)
    // conservatively scan the ORIGINAL, pre-mutation `/Extensions` value for
    // ANY indirect reference anywhere in its subtree via `collect_direct_refs`
    // — reused unchanged from the linearization closure walk, which already
    // answers exactly this "does this value contain a Reference anywhere"
    // question for its own edges — and (b) decide `has_adbe` right away if
    // `/Extensions` is already a direct dict, or extract the ref to resolve
    // one more level otherwise.
    let (extensions_ref, orphans_indirect_object) = {
        let catalog = pdf.resolve_borrowed(root_ref)?;
        // cov:ignore-start: defensive non-Dict Catalog guard. Every
        // well-formed fixture that reaches this point (a linearizable
        // document with a resolved `plan.root_ref`) has a dictionary
        // Catalog.
        let Some(catalog_dict) = catalog.as_dict() else {
            return Ok(NONE);
        };
        // cov:ignore-end
        let Some(raw_extensions) = catalog_dict.get("Extensions") else {
            return Ok(NONE);
        };

        // Deliberately conservative and shape-independent: reject on ANY
        // indirect reference found anywhere in the subtree, even ones that
        // a finer-grained analysis might prove safe (e.g., for a
        // Dictionary-shaped /Extensions, an unrelated developer-prefix key
        // next to /ADBE that would actually survive intact). Reusing the
        // same "reject the rare/unusual structure loudly" pattern applied
        // elsewhere in this crate (e.g. the conservative handling of
        // unusual extension structures) rather than special-casing which
        // reference positions are
        // provably safe.
        let mut refs = Vec::new();
        collect_direct_refs(raw_extensions, 0, &mut refs)?;
        let orphans = !refs.is_empty();

        match raw_extensions {
            Object::Dictionary(d) => {
                return Ok(CatalogAdbeStatus {
                    has_adbe: d.get("ADBE").is_some(),
                    orphans_indirect_object: orphans,
                });
            }
            Object::Reference(r) => (*r, orphans),
            // /Extensions present but neither Dict nor Ref: structurally
            // non-conformant per ISO 32000 (which defines /Extensions as a
            // dictionary), but this scans untrusted input, so it is
            // handled rather than assumed away. There is no dict to look
            // `/ADBE` up in, so `has_adbe` is unconditionally `false`;
            // `orphans` still reflects whatever `collect_direct_refs` found
            // while walking this value (generic, not hardcoded per shape).
            // See `linearize_encrypt_v5_rejects_array_extensions_with_indirect_element`.
            _ => {
                return Ok(CatalogAdbeStatus {
                    has_adbe: false,
                    orphans_indirect_object: orphans,
                })
            }
        }
    };

    // /Extensions itself is indirect: `orphans_indirect_object` is already
    // `true` here (collect_direct_refs saw the top-level Reference itself),
    // regardless of what this resolves to below — a type-agnostic result
    // that correctly rejects even when the target ISN'T a Dictionary at all,
    // matching qpdf's own resolve-and-inline behavior (which loses the
    // reference either way) rather than silently waving through a malformed
    // target. This second resolve exists purely to compute `has_adbe` by
    // mirroring qpdf's `keys.count("/ADBE")` on the resolved dict.
    let extensions = pdf.resolve_borrowed(extensions_ref)?;
    let has_adbe = extensions
        .as_dict()
        .is_some_and(|d| d.get("ADBE").is_some());
    Ok(CatalogAdbeStatus {
        has_adbe,
        orphans_indirect_object,
    })
}

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
/// Uses qpdf's two-pass layout: pass 1 omits the hint object and supplies the
/// virtual offsets for one-shot hint-table generation; pass 2 splices the
/// complete framed hint object at the reserved slot.
///
/// Returns [`LinearizedDocument`] containing both the bytes and the
/// [`LinearizedOffsets`] needed for back-patching.
///
/// With [`WriterOptions::deterministic_id`] the `/ID` is derived from an MD5
/// over the assembled layout (the same digest feeds every trailer / xref-stream
/// dict), so the identifier is reproducible across runs for identical input.
///
/// # Errors
///
/// Returns [`crate::Error::Internal`] when [`WriterOptions::deterministic_id`]
/// is combined with encrypted output ([`WriterOptions::encrypt`] or
/// [`WriterOptions::copy_encryption`]): a content-derived `/ID` cannot be
/// produced once the bytes are encrypted, because the identifier would need to
/// be known before the file encryption key that protects every string and
/// stream can be derived (the key derives from `/ID[0]`, PDF 1.7 §7.6.3.3
/// Algorithm 2). This guard mirrors only that specific restriction — qpdf itself accepts
/// a *non*-deterministic-id `/ID` alongside encryption (observed on qpdf
/// 11.9.0: `qpdf --linearize --encrypt "" "" 128 --use-aes=y -- in.pdf
/// out.pdf` succeeds, while adding `--deterministic-id` to that same command
/// fails with qpdf's own `QPDFWriter::generateID has no data for
/// deterministic ID` internal error), so linearize+encrypt on its own is not
/// rejected by this guard.
///
/// Returns [`crate::Error::Unsupported`] when the effective Adobe developer
/// extension level (`/Extensions /ADBE /ExtensionLevel` — contributed by
/// [`WriterOptions::encrypt`]'s method, [`WriterOptions::min_extension_level`],
/// or the source document itself) would change AND applying that change
/// would orphan an indirect object: an indirect reference is reachable
/// anywhere within the source Catalog's `/Extensions` subtree — the
/// `/Extensions` value itself, or any dictionary value / array element
/// nested within it at any depth (for example a direct `/ADBE` dict whose
/// own `/ExtensionLevel` is an indirect reference, not only `/ADBE` itself).
/// This is a temporary flpdf scope limitation, not a qpdf restriction:
/// dropping such a reference here would orphan its already-numbered object
/// slot (this function's object numbering is fixed by its `plan`/`renumber`
/// parameters before it runs). A source `/Extensions` that is absent, or
/// contains no indirect reference anywhere in its subtree, is always
/// handled correctly.
///
/// Returns [`crate::Error::Unsupported`] when the plan and renumber map are
/// inconsistent or a layout value does not fit its slot — for example an
/// object (catalog, page, shared, or body object) has no entry in the
/// [`RenumberMap`], the plan has no page hints or a `per_page_private_objects`
/// length that disagrees with `page_hints`, `/Size` overflows `u32`, a shared
/// object lacks a pass-1 byte length, or a hint-table field cannot represent its
/// pass-1 value.
#[cfg(test)]
pub(crate) fn write_linearized<R: Read + Seek>(
    plan: &LinearizationPlan,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
) -> Result<LinearizedDocument> {
    Ok(write_linearized_impl(plan, renumber, pdf, options, None)?.0)
}

/// Write a linearized PDF and qpdf's first-pass representation to `pass1_path`.
///
/// The pass-1 file is written after the final hint object has been generated.
/// Its body is the same throwaway first pass used for deterministic ID
/// computation, followed by qpdf's pass-1 offset comments.
#[cfg(test)]
pub(crate) fn write_linearized_with_pass1_file<R: Read + Seek>(
    plan: &LinearizationPlan,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    pass1_path: &Path,
) -> Result<LinearizedDocument> {
    Ok(write_linearized_impl(plan, renumber, pdf, options, Some(pass1_path))?.0)
}

/// Write linearized output through the canonical [`crate::PdfWriter`] route.
///
/// The public compatibility helpers above accept an already-built plan for
/// the inspection/fixture APIs.  A real writer must plan and emit the same
/// live `Pdf` after all writer settings and graph mutations have settled.  The
/// returned mapping is taken from the final local renumber map, including any
/// ObjStm relocation performed by the two-pass linearization emitter.
pub(crate) fn write_linearized_for_pdf_writer<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    pass1_path: Option<&Path>,
) -> Result<(LinearizedDocument, WriterResult)> {
    // The canonical PdfWriter route plans the same live Pdf that it emits.
    // QPDFWriter fixes the Generate ObjStm set before QPDF::optimize can
    // materialize inherited page attributes, so preparation is owned by
    // LinearizationPlan::from_pdf_with_writer_options below. The later
    // write_linearized_impl call remains idempotent for legacy plan helpers.
    let mode = if crate::writer::force_version_below_1_5(options) {
        crate::writer::ObjectStreamMode::Disable
    } else {
        options.object_streams
    };

    let mut plan_options = options.clone();
    plan_options.object_streams = mode;
    let plan = LinearizationPlan::from_pdf_with_writer_options(pdf, &plan_options)?;
    // qpdf allocates generated ObjStm placeholders before it removes page and
    // Catalog members from the mapping (QPDFWriter.cc:1970-2005, 2141-2161).
    // Count those pre-filter containers for progress even when a later filter
    // leaves one empty and therefore absent from the emitted layout. Keep this
    // traversal after plan construction: the plan's first qpdf-shaped graph
    // walk establishes stream-recovery state for malformed encrypted sources.
    let generated_object_stream_count = if mode == crate::writer::ObjectStreamMode::Generate {
        let compressible = crate::writer::object_streams::compressible_objgens_qpdf_plan(pdf)?;
        crate::writer::object_streams::even_split_into_streams(&compressible.eligible).len()
    } else {
        0
    };
    crate::writer::configure_progress_for_pdf(pdf, options, generated_object_stream_count, true)?;
    let renumber = RenumberMap::from_plan(&plan);
    // `write_linearized_impl` applies qpdf's output-only /Extensions /ADBE
    // reconciliation after the plan is frozen. Snapshot after optimization
    // and planning so permanent qpdf graph preparation remains attached to the
    // caller's Pdf, while the temporary Catalog mutation is restored on both
    // success and failure. This is the linearized counterpart of the plain
    // writer's extension-only restore boundary.
    let catalog_snapshot = crate::writer::snapshot_catalog_extensions(pdf)?;
    let result = write_linearized_impl(&plan, &renumber, pdf, options, pass1_path);
    crate::writer::restore_catalog_extensions(pdf, catalog_snapshot)?;
    result
}

/// Write the pass-1 body through qpdf's stdio-shaped buffering boundary.
///
/// qpdf writes this body through `Pl_StdioFile` backed by a buffered `FILE*`.
/// Direct `fwrite` failures are terminal, while `finish()` ignores every
/// `fflush` failure except `EBADF`, which is a logic error. Keep that behavior
/// in the core [`crate::Error`] channel instead of allowing a
/// [`crate::pipeline::PipelineError`] to escape the pipeline boundary.
fn write_pass1_stdio_body(
    writer: &mut dyn Write,
    mut body: &[u8],
    pass1_path: &Path,
) -> Result<()> {
    let mut buffered = StdioBuffer::new(writer);
    while !body.is_empty() {
        match buffered.write(body) {
            Ok(0) => {
                return Err(crate::Error::file_io(
                    "write",
                    pass1_path,
                    std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write buffered data",
                    ),
                ));
            }
            Ok(written) => body = &body[written..],
            Err(source) => {
                return Err(crate::Error::file_io("write", pass1_path, source));
            }
        }
    }

    match buffered.flush() {
        Err(source) if source.raw_os_error() == Some(EBADF_ERRNO) => Err(crate::Error::Internal(
            "linearization pass1: Pl_StdioFile::finish: stream already closed".to_string(),
        )),
        Ok(()) | Err(_) => Ok(()),
    }
}

/// Append qpdf's pass-1 debugging comments after the body pipeline has been
/// finished. `QPDFWriter.cc:2886-2900` uses unchecked `fprintf` calls followed
/// by an unchecked `fclose`, so comment write/close failures are intentionally
/// not promoted to the writer's public error channel.
fn write_pass1_debug_comments(writer: &mut dyn Write, comments: &[u8]) {
    debug_assert!(comments.len() < 4096);
    let mut buffered = StdioBuffer::new(writer);
    let _ = buffered.write(comments);
}

/// Reject manually-constructed plans whose per-page private-object lists do
/// not have one entry for every page hint. `from_pdf` always preserves this
/// alignment; keeping the check separate makes the hand-built-plan failure
/// observable without forcing a malformed plan through the full writer.
// qpdf-deviation-start: no qpdf counterpart validates a persisted plan's
// per_page_private_objects/page_hints vector-length agreement; qpdf computes
// both inline in one pass (QPDF_linearization.cc calculateLinearizationData)
// with no separate externally-constructible plan struct, and this Err arm is
// reachable only via flpdf's own #[cfg(test)] hand-built-plan entry points,
// never from parsing a real PDF.
fn validate_per_page_private_objects(plan: &LinearizationPlan) -> Result<()> {
    if plan.per_page_private_objects.len() != plan.page_hints.len() {
        return Err(crate::Error::Unsupported(format!(
            "linearization writer: per_page_private_objects length ({}) does not \
                 match page_hints length ({}) — plan invariant violated",
            plan.per_page_private_objects.len(),
            plan.page_hints.len()
        )));
    }
    Ok(())
}
// qpdf-deviation-end

fn write_linearized_impl<R: Read + Seek>(
    plan: &LinearizationPlan,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
    options: &WriterOptions,
    pass1_path: Option<&Path>,
) -> Result<(LinearizedDocument, WriterResult)> {
    // `--deterministic-id` and `--static-id` are mutually exclusive: a
    // content-derived `/ID` and qpdf's fixed test constant cannot both be the
    // identifier. The flat (`crate::writer::emit_canonical_pdf`) path
    // rejects the combination; mirror it here so the public linearization API
    // does not silently let the deterministic branch win over `static_id`.
    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }

    // Finalize the file identifier exactly once here — before the plan/
    // renumber-map rebuild below, before `Optimization::prepare_for_linearized_write`,
    // and before the two layout passes — and store it back on the working
    // trailer. The Part-1 trailer and every split xref/trailer then read this
    // single value, so one linearized output carries one consistent /ID across
    // both passes.
    //
    // Computed this early (rather than just before the layout passes,
    // where this block used to sit) so `/ID[0]` is available before the
    // renumber map is consumed: the file encryption key derives from
    // `/ID[0]` (PDF 1.7 §7.6.3.3 Algorithm 2), and qpdf itself computes
    // `/ID` once, early, via `generateID()`'s idempotent guard
    // (`QPDFWriter::write` calls it before `unparseObject`-ing the trailer).
    // This computation depends only on `options` and the source trailer's
    // `/ID`/`/Info` — neither the plan/renumber rebuild nor
    // `Optimization::prepare_for_linearized_write` below ever mutates the
    // trailer, so moving it here does not change any output byte for an
    // existing (non-encrypting) caller.
    //
    // Capture qpdf's deterministic-`/ID` seed inputs from the ORIGINAL trailer
    // BEFORE the all-zero placeholder overwrites `/ID` below. `/ID[0]` is the
    // preserved permanent identifier and the `/Info`-derived suffix feeds the
    // seed; reading either after the placeholder is installed would mistake the
    // 16 zero bytes for a real source `/ID[0]` and corrupt the result.
    let source_trailer_handle = pdf.trailer().shallow_copy()?;
    let source_id0 = crate::writer::source_permanent_id_handle(&source_trailer_handle);
    let (det_id_source_id0, det_id_info_suffix): (Option<Vec<u8>>, Vec<u8>) =
        if options.deterministic_id {
            let suffix = crate::writer::deterministic_id_info_suffix(pdf);
            (source_id0.clone(), suffix)
        } else {
            (None, Vec::new())
        };
    let pass1_id = linearization_pass1_id(source_id0.as_deref());
    let finalized_id = finalize_linearized_id(
        options,
        source_id0.as_deref(),
        det_id_source_id0.as_deref(),
        options.copy_encryption.as_ref(),
    );
    // Extract `/ID[0]` now, before `finalized_id` moves into `source_trailer`,
    // for `build_encryption_context` below (PDF 1.7 §7.6.3.3 Algorithm 2 uses
    // `/ID[0]` as a salt, and the trailer's `/ID[0]` must carry the same bytes
    // so a reader can re-derive the file key). `finalize_linearized_id` always
    // returns a 2-element string array — every branch (deterministic
    // placeholder, static-id, default) constructs one — so this is an
    // internal-invariant check, not a reachable error for well-formed input.
    //
    // NOTE: when `options.deterministic_id` is set, this array's element 0 is
    // an ALL-ZERO PLACEHOLDER (`finalize_linearized_id`'s first branch above),
    // not real key material — the actual content-derived identifier is only
    // computed after the bytes exist, and is then either direct-written
    // (classic path) or back-patched in place (ObjStm/xref-stream path) — see
    // `finalize_linearized_id`'s doc. `id0` is extracted unconditionally
    // here, so it CAN transiently hold that placeholder. It is only safe to
    // feed into `build_encryption_context` below because the
    // `deterministic_id && encrypting` guard immediately below this block
    // returns `Err` first whenever both are set — see the `debug_assert!` at
    // this function's `options.encrypt` consumption site, which restates that
    // invariant at the point it actually matters.
    let id0: Vec<u8> = finalized_id
        .as_array()
        .and_then(|values| values.first())
        .and_then(Object::as_string)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| {
            // cov:ignore-start: unreachable — every branch of finalize_linearized_id
            // constructs a well-formed 2-element string array (see its own body)
            crate::Error::Unsupported(
                "linearization writer: finalize_linearized_id did not return a \
                 well-formed /ID array"
                    .to_string(),
            )
        })?; // cov:ignore-end
    let finalized_id_handle = id_object_to_handle(&finalized_id)?;
    let pass1_id_handle = id_object_to_handle(&pass1_id)?;
    source_trailer_handle.replace_key(b"/ID", finalized_id_handle)?;
    let pass1_source_trailer = source_trailer_handle.shallow_copy()?;
    pass1_source_trailer.replace_key(b"/ID", pass1_id_handle)?;

    // QPDFWriter::setEncryptionParameters and
    // QPDFWriter::copyEncryptionParameters call generateID() before the
    // linearized pass can produce deterministic ID data. Translate that
    // qpdf logic_error rather than treating the combination as an unsupported
    // feature.
    if options.deterministic_id && (options.encrypt.is_some() || options.copy_encryption.is_some())
    {
        return Err(crate::writer::generate_id_without_data());
    }

    // `plan`/`renumber` are built from a separate `Pdf` handle opened on the
    // same source bytes (every real caller — the CLI, and this module's own
    // `build_linearized()` test helper — re-opens the input for writing rather
    // than reusing the planning handle: "Re-open the PDF so write_linearized
    // can seek/read objects independently"). Run qpdf's complete optimization
    // preparation prefix here too, on THIS handle, so direct `/Outlines`,
    // page-tree repairs, and inherited-attribute minting happen in the same
    // order and allocate the same object numbers the plan assumed. Idempotent:
    // a no-op if `pdf` was already prepared (e.g. a caller that reuses one
    // handle for both steps). Runs after the option guards above so an invalid
    // option combination returns its error without mutating the caller's `Pdf`
    // first.
    crate::optimization::Optimization::prepare_for_linearized_write(pdf)?;

    // Reconcile the caller-built plan/renumber pair with the writer's effective
    // object-stream mode. The historical `from_pdf(bool)` API maps `false` to
    // Disable, while `WriterOptions::default()` is Preserve; source-ObjStm
    // Preserve has different stale-generation and container-routing rules, so
    // reusing the Disable partitions can reject or mis-layout the file. Rebuild
    // both structures together whenever their recorded planning mode differs.
    //
    // A forced sub-1.5 header also makes the effective mode Disable: object and
    // cross-reference streams are PDF 1.5 features and qpdf will not emit them
    // under a forced version it must not exceed (observed on qpdf 11.9.0:
    // `--linearize --object-streams=generate --force-version=1.4` yields no
    // `/ObjStm` and a classic xref table at header 1.4). In that case normalize
    // the write options too, so plan, renumbering, and physical output all use
    // the same classic layout.
    let effective_object_stream_mode = if crate::writer::force_version_below_1_5(options) {
        crate::writer::ObjectStreamMode::Disable
    } else {
        options.object_streams
    };
    let must_rebuild_plan = plan.object_stream_mode != effective_object_stream_mode;
    let rebuilt = if must_rebuild_plan {
        let rebuilt_plan =
            LinearizationPlan::from_pdf_with_object_stream_mode(pdf, effective_object_stream_mode)?;
        let rebuilt_renumber = RenumberMap::from_plan(&rebuilt_plan);
        Some((rebuilt_plan, rebuilt_renumber))
    } else {
        None
    };
    let must_normalize_options = options.object_streams != effective_object_stream_mode;
    let normalized_options = if must_normalize_options {
        Some(WriterOptions {
            object_streams: effective_object_stream_mode,
            ..options.clone()
        })
    } else {
        None
    };
    let (plan, renumber) = match rebuilt.as_ref() {
        Some((rebuilt_plan, rebuilt_renumber)) => (rebuilt_plan, rebuilt_renumber),
        None => (plan, renumber),
    };
    let options = normalized_options.as_ref().unwrap_or(options);

    // qpdf's linearization maps discard generations only after asserting that
    // every surviving object number is unique. Generate, and Preserve on an
    // ObjStm source, have already removed stale generations while planning;
    // standard Disable/Preserve retain them and must reject the file here.
    // discard_lower_generations (xref.rs) only removes duplicate rows from
    // the raw xref table at load time; it does not resolve a reference to a
    // generation that never had its own xref row (see
    // reject_multiple_generations's own doc).
    reject_multiple_generations(plan)?;

    // ------------------------------------------------------------------
    // Pre-compute values that do not change across the two layout passes.
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

    // Whether this write schedules any ObjStm container/member relocation —
    // true for Generate (always builds fresh containers) or for Preserve on a
    // source that already carries object streams; always false for Disable
    // and for Preserve on an ObjStm-free source. Computed once here (and
    // reused below both by the encrypt guard and by the 1.5 version floor)
    // from the writer-filtered batch plan, so it reflects every path that can
    // make a batch non-empty, not just `ObjectStreamMode::Generate`.
    let emits_object_streams = !resolved_batch_plan.open_document_batches.is_empty()
        || !resolved_batch_plan.part3_batches.is_empty()
        || !resolved_batch_plan.part4_batches.is_empty();

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
    // qpdf's root /Pages user can contain several nested Pages nodes (see
    // `LinearizationPlan::part4_objects`); every member remaining in
    // `part4_rest` is promoted ahead of the rest of part9, not only the
    // Catalog's direct /Pages object. Mirror that full set here so none of
    // those promoted nodes is swept into the post-container pass below.
    let part9_pages: BTreeSet<ObjectRef> = plan
        .optimization
        .as_ref()
        .map(|optimization| optimization.objects_for_root_key(b"Pages"))
        .filter(|pages| !pages.is_empty())
        .unwrap_or_else(|| plan.pages_tree_ref.into_iter().collect());
    let second_half_post_plain: BTreeSet<ObjectRef> = plan
        .part4_rest
        .iter()
        .chain(&plan.part9_outline_objects)
        .copied()
        .filter(|r| {
            !part4_member_set.contains(r) && !part9_pages.contains(r) && Some(*r) != plan.info_ref
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
    let mut first_half_post_plain: BTreeSet<ObjectRef> = plan
        .part6_outline_objects
        .iter()
        .copied()
        .filter(|object| !first_half_member_set.contains(object))
        .collect();
    if let Some(pre_objects) = plan
        .optimization
        .as_ref()
        .and_then(|optimization| optimization.pre_optimization_object_refs())
    {
        first_half_post_plain.extend(
            plan.part2_objects
                .iter()
                .chain(&plan.part3_objects)
                .chain(&plan.part4_open_document_plain)
                .chain(&plan.part6_outline_objects)
                .copied()
                .filter(|object| {
                    !pre_objects.contains(object) && !first_half_member_set.contains(object)
                }),
        );
    }
    // Open-document batches are numbered FIRST in the first half (right after
    // the catalog, before the hint); Part-3 batches are numbered last within
    // the first half (qpdf packs the first-page shared dicts + /Pages tree +
    // /Info there); Part-4 batches are interleaved among the second-half
    // objects at their part position.
    //
    // qpdf supports linearize+encrypt+ObjStm. Placement must therefore happen
    // before the `/Encrypt` slot is inserted; the slot reservation below then
    // shifts the placed map and the derived ObjStm layout is built afterwards
    // from the shifted map.
    let relocation = if emits_object_streams {
        local_renumber.place_objstm_members_per_half(
            &resolved_batch_plan.open_document_batches,
            &resolved_batch_plan.part3_batches,
            &part4_members,
            &second_half_anchors,
            &second_half_post_plain,
            &first_half_post_plain,
        )
    } else {
        ObjStmRelocation::default()
    };
    let mut container_numbers = relocation.container_numbers.clone();

    // Build the encryption context and reserve the `/Encrypt` dict's object
    // slot BEFORE anything below reads `param_dict_ref()`, `hint_stream_slot()`,
    // or `len()` off `renumber` — `reserve_encrypt_dict_slot` shifts every
    // already-assigned slot at/after the (old) hint-stream position by one,
    // and `Part1Bytes::build`, `hint_stream_new_num`, `total_count`, and the
    // Part-1 dict serialization all read those numbers off the SAME
    // `local_renumber` this mutates — every one of those reads happens
    // through the `renumber` shadow assigned right after this block, so
    // placing the reservation here (rather than scattered at each read site)
    // covers all of them at once.
    //
    // ObjStm placement runs before this reservation. Inserting the encryption
    // sentinel at the hint slot then shifts only the first-half objects that
    // follow it; the container numbers below are adjusted by the same amount.
    //
    // `existing_max` only feeds `build_encryption_context`'s internal
    // `existing_max + 1` slot guess. That guess is immediately discarded
    // below in favor of `reserve_encrypt_dict_slot`'s qpdf-aligned
    // mid-sequence placement (`ctx.encrypt_ref` is overwritten), so
    // `existing_max`'s exact value has no other effect on the returned
    // context — any non-overflowing count is safe here.
    //
    // Explicit encryption and copied source encryption share the same qpdf
    // output slot and emission machinery. The copy branch supplies the
    // authenticated donor key instead of deriving a new key from passwords.
    //
    // `encrypt_ctx` is threaded into every `do_write_pass` call below, which
    // emits `ctx.encrypt_dict` as a plaintext indirect object right after the
    // catalog/open-document-plain objects (mirrors qpdf's `writeLinearized`
    // calling `writeEncryptionDictionary()` right after `part4_end_marker`,
    // unconditionally on `m->encrypted` — QPDFWriter.cc:2793-2796). Writing
    // it into the trailer, and applying it to per-object strings/streams, are
    // later steps that consume this value.
    let encrypt_ctx: Option<crate::writer::EncryptionContext> =
        if let Some(params) = options.encrypt.as_ref() {
            // `id0` (extracted above, before the `deterministic_id &&
            // encrypting` guard runs) must never be the all-zero placeholder
            // here: reaching this branch means `options.encrypt.is_some()`,
            // and the guard above already returns `Err` before this point
            // whenever `deterministic_id` also holds. Restated here,
            // self-enforcing, in case a future edit reorders the guard
            // relative to this block.
            debug_assert!(
                !options.deterministic_id,
                "deterministic_id && encrypting must have already been rejected \
                 by the guard above `write_linearized`'s /ID finalization — \
                 reaching here with deterministic_id set would derive the file \
                 encryption key from an all-zero /ID[0] placeholder"
            );
            let existing_max: u32 = local_renumber.len().try_into().map_err(|_| {
                // cov:ignore-start: requires > 2^32 objects — impossible in practice
                crate::Error::Unsupported(
                    "linearization writer: object count overflows u32 for /Encrypt slot \
                     reservation"
                        .to_string(),
                )
            })?; // cov:ignore-end
                 // Resolve /Metadata up front for --cleartext-metadata support, mirroring
                 // the full-rewrite writer's own gating (`!params.encrypt_metadata`).
            let metadata_ref = if params.encrypt_metadata {
                None
            } else {
                crate::writer::resolve_metadata_stream_ref(pdf)
            };
            let ctx_result = crate::writer::build_encryption_context(
                options,
                params,
                existing_max,
                metadata_ref,
                &id0,
            );
            let mut ctx = ctx_result?;
            ctx.encrypt_ref = local_renumber.reserve_encrypt_dict_slot();
            for container_number in &mut container_numbers {
                if *container_number >= ctx.encrypt_ref.number {
                    *container_number += 1;
                }
            }
            Some(ctx)
        } else if let Some(source) = options.copy_encryption.as_ref() {
            // cov:ignore-start: a supported in-memory PDF cannot contain 2^32 objects;
            // the conversion failure is an internal capacity guard only.
            let existing_max: u32 = local_renumber.len().try_into().map_err(|_| {
                crate::Error::Unsupported(
                    "linearization writer: object count overflows u32 for /Encrypt slot \
                     reservation"
                        .to_string(),
                )
            })?;
            // cov:ignore-end
            let encrypt_metadata = source
                .encrypt_dict
                .get("EncryptMetadata")
                .and_then(Object::as_bool)
                .unwrap_or(true);
            let metadata_ref = if encrypt_metadata {
                None
            } else {
                crate::writer::resolve_metadata_stream_ref(pdf)
            };
            let mut ctx = crate::writer::build_copy_encryption_context(
                source,
                options,
                existing_max,
                metadata_ref,
            )?;
            ctx.encrypt_ref = local_renumber.reserve_encrypt_dict_slot();
            for container_number in &mut container_numbers {
                if *container_number >= ctx.encrypt_ref.number {
                    *container_number += 1;
                }
            }
            Some(ctx)
        } else {
            None
        };
    let mut encrypted_string_emitter = encrypt_ctx
        .as_ref()
        .map(EncryptedStringEmitter::from_context);

    // Draw the hint stream's AES IV once for this whole invocation and use it
    // while constructing the one complete hint object — see that function's
    // doc for qpdf's encrypt-once/replay boundary. `--static-aes-iv`
    // keeps using the same fixed test vector every other AES call in this
    // writer already uses, so that path is byte-for-byte unchanged. When no
    // AES cipher is in play (RC4, or `encrypt_ctx` is `None`) the value is
    // never read; `[0u8; 16]` is a harmless placeholder.
    let hint_stream_aes_iv: [u8; 16] = match &encrypt_ctx {
        Some(ctx) if ctx.static_aes_iv => crate::pipeline::aes::static_initialization_vector(),
        Some(ctx) if crate::writer::cipher_needs_aes_iv(ctx.cipher) => {
            let mut iv = [0u8; 16];
            // cov:ignore-start: defensive — the OS CSPRNG does not fail on
            // any platform this crate's test suite runs on; mirrors the
            // same untested-in-practice getrandom failure arm in the writer's
            // canonical stream pipeline.
            getrandom::fill(&mut iv).map_err(|e| {
                crate::Error::Unsupported(format!(
                    "OS CSPRNG (getrandom) unavailable for AES IV generation: {e}"
                ))
            })?;
            // cov:ignore-end
            iv
        }
        _ => [0u8; 16],
    };

    let renumber: &RenumberMap = &local_renumber;

    // Floor the header to 1.5 only when the output actually carries an ObjStm
    // container (qpdf raises the minimum on real emission, not on mode). When
    // all batch lists are empty the placement early-returned and no container
    // is written, so the non-ObjStm linearized goldens stay at the 1.2 floor.
    //
    // Adobe developer-extension propagation (qpdf QPDFWriter.cc L1355-1450
    // `addDeveloperExtension`, and the pairwise `setMinimumPDFVersion`
    // contributions at L806-815 that give V=5 R=6/R=5 `--encrypt` their
    // `/Extensions /ADBE /ExtensionLevel` 8/3 floor) — mirrors
    // `crate::writer::emit_canonical_pdf_inner`'s handling
    // (writer.rs:3154-3238), reusing the SAME pairwise-combine function so
    // the injected `/BaseVersion` always agrees with the header version
    // computed from the identical `(eff_version, eff_ext)` pair. `source_ver`
    // is cloned to an owned `String` first so the borrow-checker sees no
    // conflict between the immutable `pdf.version()` read and the `&mut self`
    // `pdf.adobe_extension_level()` call just below it (mirrors the flat
    // writer's own `source_ver`/`source_ext` locals).
    let source_ver = pdf.version().to_string();
    let source_ext = pdf.adobe_extension_level().unwrap_or(0);
    let (eff_version, eff_ext) =
        effective_pdf_version_and_ext(&source_ver, source_ext, options, true, emits_object_streams);
    let adbe_status = resolve_catalog_adbe_status(pdf)?;
    if eff_ext > 0 || adbe_status.has_adbe {
        // See `CatalogAdbeStatus::orphans_indirect_object`'s doc for why any
        // indirect reference reachable anywhere within the source
        // `/Extensions` subtree is rejected here rather than inlined:
        // unlike the flat writer, this function's `plan`/`renumber` were
        // already frozen by the caller from a separate `Pdf` handle before
        // this point, so dropping such a reference here would orphan its
        // already-counted slot while still emitting it with stale content.
        if adbe_status.orphans_indirect_object {
            return Err(crate::Error::Unsupported(
                "linearize: a source Catalog /Extensions subtree containing \
                 an indirect reference (the /Extensions value itself, or \
                 any nested dictionary value / array element within it) is \
                 not yet supported when the effective Adobe extension \
                 level changes; inline /Extensions in the source or file a \
                 follow-up if you need this combination"
                    .to_string(),
            ));
        }
        if eff_ext > 0 {
            inject_adbe_extension(pdf, eff_version, eff_ext)?;
        } else {
            strip_adbe_extension(pdf)?;
        }
    }
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
    // Build the ObjStm layout from the relocated map (stable across both
    // layout passes). Container + member numbers now live INSIDE the
    // renumber map (relocation appended them), so the pair tables never
    // shift between passes — only the surrounding byte offsets do.
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
    // - Preserve: source ObjStm containers are validated against their members,
    //   but the hint table compares them in the physical output-number space,
    //   hence `(0, container_new_num)` alongside renumbered plain objects.
    let container_shared_sort_key: std::collections::BTreeMap<u32, (u8, u32)> = match options
        .object_streams
    {
        crate::writer::ObjectStreamMode::Preserve => {
            let source_container_by_member: std::collections::BTreeMap<ObjectRef, u32> = pdf
                .source_xref_entries()
                .into_iter()
                .filter_map(|(object_ref, offset)| match offset {
                    crate::XrefEntry::Compressed { stream, .. } => Some((object_ref, stream)),
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
                let _source_container_number =
                    preserved_source_container_number(container, &source_container_by_member)?;
                keys.insert(
                    container.container_new_num,
                    (0, container.container_new_num),
                );
            }
            keys
        }
        _ => {
            use crate::linearization::plan::objstm_membership_linearized_with_eligibility;
            let assigned = plan.renumber_assigned_refs();
            let membership = objstm_membership_linearized_with_eligibility(
                pdf,
                &assigned,
                plan.optimization
                    .as_ref()
                    .and_then(|optimization| optimization.generate_objstm_eligible()),
            )?; // cov:ignore: closing line of a multi-line call; llvm-cov misattributes the hit count to the previous line, not an untested branch
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

    let info_new_ref: Option<ObjectRef> = source_trailer_handle
        .try_get_key(b"/Info")?
        .object_ref()
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
    // Outlines Hint Table inputs (qpdf in_outlines / calculateHOutline).
    // `None` when the document has no outlines, in which case no `/O` key or
    // outline table is emitted (byte-identical to the no-outline path).
    let outlines = plan
        .optimization
        .as_ref()
        .map(|optimization| optimization.objects_for_root_key(b"Outlines"))
        .unwrap_or_default();
    let outline_info = compute_outline_hint_info(&outlines, pdf, renumber, &objstm_layout)?;
    // qpdf routes the primary hint stream through its global stream-compression
    // setting as well: Preserve and Uncompress emit the raw bit-packed table
    // without `/Filter`; Compress emits `/FlateDecode`.
    let structural_streams_filtered =
        matches!(effective_stream_policy(options), Some(CompressStreams::Yes));
    // ------------------------------------------------------------------
    // Build qpdf's first-pass representation unconditionally. It is the source
    // for hint-table offsets and lengths, and is also reused for deterministic
    // ID hashing and explicit pass-1 output. For deterministic IDs, compute
    // qpdf's content-derived identifier up front, then direct-write it in the
    // final pass on the classic path (qpdf's 2-pass scheme).
    //
    // qpdf seeds the linearized `--deterministic-id` from its *first* write pass
    // — a throwaway buffer with an empty parameter dict, no hint stream, and an
    // unresolved first-page xref (`QPDFWriter::writeLinearized` →
    // `computeDeterministicIDData`, qpdf 11.9.0; the hint stream is written only
    // afterwards). That pass-1 buffer is loop-invariant (it carries no hint
    // stream, so it never depends on a later hint-object splice), so build it once here and
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
    let pass1_part1 = build_pass1_part1(&part1);
    let pass1_output = do_write_pass(
        plan,
        renumber,
        pdf,
        &pass1_part1,
        catalog_new_ref,
        hint_stream_new_num,
        total_count,
        info_new_ref,
        first_page_object_new_num,
        None,
        structural_streams_filtered,
        &pass1_source_trailer,
        &objstm_layout,
        &relocation,
        options,
        true,
        None,
        encrypt_ctx.as_ref(),
        encrypted_string_emitter.as_mut(),
    )?; // cov:ignore: pass-1 mode uses the same write path as the successful final pass while omitting only the hint object.

    let classic_det_id: Option<(Vec<u8>, [u8; 16])> = if options.deterministic_id {
        let pass1_bytes = &pass1_output.bytes;
        // Whole-buffer digest: a linearized file repeats `/ID` at several
        // sites, so there is no single `[` cutoff; pass the last index as the
        // inclusive end (matching the prior patch step's digest range).
        Some(crate::writer::compute_deterministic_id(
            pass1_bytes,
            pass1_bytes.len() - 1,
            &det_id_info_suffix,
            det_id_source_id0.as_deref(),
        ))
    } else {
        None
    };

    // ------------------------------------------------------------------
    // Build every hint table once from qpdf's pass-1 output. Pass 1 omits the
    // hint object, so these offsets are already the virtual coordinates that
    // qpdf stores in the hint tables.
    // ------------------------------------------------------------------
    let xref_offsets = &pass1_output.xref_offsets;
    let hint_stream_offset = pass1_output.hint_stream_offset;
    let last_xref_offset = pass1_output.last_xref_offset;
    // ------------------------------------------------------------------
    // Compute per-object byte lengths from pass 1.
    // Use the xref keyword offset (not first_entry_offset) for length computation.
    // ------------------------------------------------------------------
    let byte_lengths = compute_byte_lengths(
        xref_offsets,
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
    // aligned with `page_hints` (one entry per page).
    validate_per_page_private_objects(plan)?;

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
    // is constant across both passes, hint_stream_offset is stable.
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

    // Shared object table fields.
    //
    // The shared hint table covers all plan.shared_hints entries (part2
    // entries first, then part3 entries).  Per qpdf's checkHSharedObject,
    // the table starts at the first-page section's first object (part2[0] =
    // page dict), so we use shared_hints[0] for the location field.
    // Collect byte lengths for all shared hint entries in plan order.
    //
    // Resolve renumber + pass-1 byte-length lookups strictly:
    // a missing entry indicates a planner / renumber inconsistency or
    // a pass-1 coverage bug, both of which would silently produce
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
    let shared_section_lens: Vec<u64> = folded_shared
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
                let len = byte_lengths
                    .get(&h.object_ref.number)
                    .copied()
                    .ok_or_else(|| {
                        crate::Error::Unsupported(format!(
                            "shared hint container (new #{}) has no probed byte length",
                            h.object_ref.number
                        ))
                    })?;
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

    // Location (item 2): qpdf's pass-1 virtual byte offset of the first
    // Part-8 shared object. In the final file qpdf's
    // `adjusted_offset(location)` adds the exact `/H[1]` bytes that are
    // spliced between Part 1 and this object.
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
        so_table.header.location = first_part8_off as u64;
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

    // Patch the Outlines Hint Table (qpdf calculateHOutline): fill the
    // per-pass offset/length for the first outline unit (see
    // `build_outline_hint_table`).
    let outline_table = outline_info
        .as_ref()
        .map(|info| build_outline_hint_table(info, xref_offsets, &byte_lengths))
        .transpose()?;

    // Re-encode hint stream with patched tables.
    let new_hint_bytes = encode_hint_stream(&po_table, &so_table, outline_table.as_ref())?;
    let new_hint_payload = if structural_streams_filtered {
        new_hint_bytes.compressed
    } else {
        new_hint_bytes.uncompressed
    };
    let new_shared_s = new_hint_bytes.shared_section_offset_in_uncompressed;
    let new_outline_o = new_hint_bytes.outline_section_offset_in_uncompressed;

    // qpdf frames and encrypts the complete hint object once after pass 1.
    // Pass 2 receives this exact buffer and splices it without re-encoding
    // the payload or drawing another IV.
    let mut hint_stream_object = Vec::new();
    append_hint_stream_object(
        &mut hint_stream_object,
        ObjectRef::new(hint_stream_new_num, 0),
        &new_hint_payload,
        new_shared_s,
        new_outline_o,
        structural_streams_filtered,
        encrypt_ctx.as_ref(),
        hint_stream_aes_iv,
    )?; // cov:ignore: internally-built hint payload and encryption context make this only a defensive propagation boundary.

    // Final pass: write the layout with the exact hint object generated
    // above. The pass-1 virtual offsets and the spliced object length are
    // therefore related by qpdf's adjusted-offset rule.
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
    let final_output = do_write_pass(
        plan,
        renumber,
        pdf,
        &part1,
        catalog_new_ref,
        hint_stream_new_num,
        total_count,
        info_new_ref,
        first_page_object_new_num,
        Some(&hint_stream_object),
        structural_streams_filtered,
        &source_trailer_handle,
        &objstm_layout,
        &relocation,
        options,
        false,
        id_writer,
        encrypt_ctx.as_ref(),
        encrypted_string_emitter.as_mut(),
    )?; // cov:ignore: pass 2 reuses the validated plan and fixed layout after pass 1 succeeds; this is only defensive error propagation.
    let LinearizedPassOutput {
        bytes: mut final_bytes,
        xref_offsets: final_xref_offsets,
        first_page_xref_offset: final_first_page_xref_offset,
        hint_stream_offset: final_hint_stream_offset,
        hint_stream_obj_total_len: final_hint_stream_obj_total_len,
        end_of_first_page_offset: final_end_of_first_page_offset,
        last_xref_offset: final_last_xref_keyword_offset,
        last_xref_first_entry_offset: final_last_xref_first_entry_offset,
        first_trailer_prev_range: final_first_trailer_prev_range,
        id_ranges: final_id_ranges,
    } = final_output;

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

    if let Some(pass1_path) = pass1_path {
        let pass1_bytes = &pass1_output.bytes;
        let pass1_hint_stream_offset = pass1_output.hint_stream_offset;
        let pass1_main_xref_offset = pass1_output.last_xref_offset;
        let second_xref_end = if objstm_layout.is_empty() {
            0
        } else {
            let marker = b"startxref\n";
            pass1_bytes
                .windows(marker.len())
                .rposition(|window| window == marker)
                // cov:ignore-start: unreachable internal invariant — xref-stream
                // pass 1 always ends with the startxref marker emitted by do_write_pass.
                .ok_or_else(|| {
                    crate::Error::Unsupported(
                        "linearization writer: pass-1 xref-stream output has no trailing \
                         startxref marker (internal invariant violated)"
                            .to_string(),
                    )
                })?
            // cov:ignore-end
        };
        let debug_comments = format!(
            "% hint_offset={pass1_hint_stream_offset}\n\
             % hint_length={final_hint_stream_obj_total_len}\n\
             % second_xref_offset={pass1_main_xref_offset}\n\
             % second_xref_end={second_xref_end}\n"
        );
        let mut pass1_file = std::fs::File::create(pass1_path)
            .map_err(|source| crate::Error::file_io("open", pass1_path, source))?;
        write_pass1_stdio_body(&mut pass1_file, pass1_bytes, pass1_path)?;
        write_pass1_debug_comments(&mut pass1_file, debug_comments.as_bytes());
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
        xref_offsets: final_xref_offsets.clone(),
        first_trailer_prev_range: final_first_trailer_prev_range,
        dict_writable_region: part1_dict_region,
    };

    let old_to_new = renumber
        .iter_in_layout_order()
        .map(|(new_ref, old_ref)| (old_ref, new_ref))
        .collect();

    let mut written_xref = final_xref_offsets
        .iter()
        .map(|(&number, &offset)| {
            (
                ObjectRef::new(number, 0),
                crate::XrefEntry::Uncompressed {
                    offset: offset as u64,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    if !objstm_layout.is_empty() {
        // cov:ignore-start: every non-empty ObjStm layout pass records its first-page xref offset;
        // a missing value is an internal invariant failure, not a supported input shape.
        let first_xref_offset = final_first_page_xref_offset.ok_or_else(|| {
            crate::Error::Unsupported(
                "linearization result: missing first-page xref offset".to_string(),
            )
        })?;
        // cov:ignore-end
        written_xref.insert(
            ObjectRef::new(relocation.first_xref_slot, 0),
            crate::XrefEntry::Uncompressed {
                offset: first_xref_offset as u64,
            },
        );
        written_xref.insert(
            ObjectRef::new(relocation.main_xref_slot, 0),
            crate::XrefEntry::Uncompressed {
                offset: final_last_xref_keyword_offset as u64,
            },
        );
        for container in objstm_layout
            .open_document
            .iter()
            .chain(&objstm_layout.part3)
            .chain(&objstm_layout.part4)
        {
            for (index, &(_original, new_ref)) in container.members.iter().enumerate() {
                written_xref.insert(
                    new_ref,
                    crate::XrefEntry::Compressed {
                        stream: container.container_new_num,
                        index: index as u32,
                    },
                );
            }
        }
    }

    Ok((
        LinearizedDocument {
            bytes: final_bytes,
            offsets,
        },
        WriterResult::new(old_to_new, written_xref),
    ))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::plan::LinearizationPlan;
    use crate::writer::{WriterOptions, DETERMINISTIC_ID_ARRAY_LEN};
    use crate::{Dictionary, Pdf};
    use std::io::Cursor;

    struct FinishErrorWriter {
        errno: i32,
        bytes: Vec<u8>,
    }

    struct ZeroWriter;

    impl std::io::Write for ZeroWriter {
        fn write(&mut self, _data: &[u8]) -> std::io::Result<usize> {
            Ok(0)
        }

        // cov:ignore-start: the zero-progress write returns before this test double can be flushed
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        // cov:ignore-end
    }

    impl std::io::Write for FinishErrorWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from_raw_os_error(self.errno))
        }
    }

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

    // -----------------------------------------------------------------------
    // Fixture: minimal single-page PDF whose trailer carries a custom,
    // non-writer-owned entry (`/CustomTrailer`) that indirectly references a
    // stream object otherwise unreachable from `/Root`. Probes qpdf's
    // `unparseChild` rule for trailer values (`QPDFWriter.cc:1143-1155`):
    // an indirect child is always written as `"N 0 R"`, never dereferenced.
    //
    // Object layout:
    //   1 0 obj – Catalog  (/Root)
    //   2 0 obj – Pages node
    //   3 0 obj – Page dict (Kids[0])
    //   4 0 obj – custom stream, reachable only via trailer /CustomTrailer
    // -----------------------------------------------------------------------
    fn tiny_pdf_with_custom_trailer_stream_bytes() -> Vec<u8> {
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

        let off4 = pdf.len() as u64;
        let stream_data = b"CUSTOM STREAM PAYLOAD";
        pdf.extend_from_slice(
            format!(
                "4 0 obj\n<< /Type /CustomStream /Length {} >>\nstream\n",
                stream_data.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(stream_data);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let xref_start = pdf.len() as u64;
        let xref = format!(
            "xref\n0 5\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \n",
            off1, off2, off3, off4,
        );
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size 5 /Root 1 0 R /CustomTrailer 4 0 R >>\nstartxref\n{}\n%%EOF\n",
            xref_start,
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    fn open_tiny_pdf_with_custom_trailer_stream() -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(tiny_pdf_with_custom_trailer_stream_bytes()))
            .expect("custom-trailer-stream fixture should parse")
    }

    fn open_encrypted_three_page_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/encrypted-r4-three-page.pdf");
        let bytes = std::fs::read(&path).expect("encrypted three-page fixture must exist");
        Pdf::open(Cursor::new(bytes)).expect("encrypted three-page PDF should parse")
    }

    fn open_cleartext_metadata_encrypted_three_page_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let mut input = Pdf::open(Cursor::new(
            include_bytes!("../../../../tests/fixtures/compat/three-page.pdf").to_vec(),
        ))
        .expect("three-page fixture should parse");
        let mut params = crate::encryption::EncryptParams::v4_aes128(Vec::new(), b"owner".to_vec());
        params.encrypt_metadata = false;
        let options = WriterOptions {
            encrypt: Some(params),
            ..WriterOptions::default()
        };
        let mut encrypted = Vec::new();
        crate::writer::emit_canonical_pdf(&mut input, &mut encrypted, &options)
            .expect("cleartext-metadata encrypted donor should write");
        Pdf::open(Cursor::new(encrypted)).expect("generated donor should parse")
    }

    /// Minimal one-page PDF whose catalog contains a direct `/Outlines`
    /// dictionary. qpdf's optimization prefix makes it indirect before both
    /// planning and writing.
    fn direct_outlines_pdf_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /Outlines << /Type /Outlines /Count 0 >> >>\nendobj\n",
        );

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    fn pdf_without_root() -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 2\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 2 >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
        );
        Pdf::open(Cursor::new(pdf)).expect("rootless PDF should parse")
    }

    fn build_linearized() -> LinearizedDocument {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = open_tiny_pdf();
        write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default())
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
    fn canonical_linearized_copy_encryption_covers_objstm() {
        let mut pdf = open_encrypted_three_page_pdf();
        let source = pdf
            .writer_copy_encryption_source()
            .expect("authenticated donor snapshot")
            .expect("encrypted fixture must provide copy parameters");
        let options = WriterOptions {
            object_streams: crate::writer::ObjectStreamMode::Generate,
            copy_encryption: Some(source),
            ..WriterOptions::default()
        };

        let (mut document, _) = write_linearized_for_pdf_writer(&mut pdf, &options, None)
            .expect("canonical linearized copy-encryption write");
        document.back_patch().expect("back-patch final document");

        assert!(
            document
                .bytes
                .windows(b"/Type /ObjStm".len())
                .any(|window| { window == b"/Type /ObjStm" }),
            "generated copy-encrypted linearization must contain an object stream"
        );
        assert!(
            document
                .bytes
                .windows(b"/Encrypt".len())
                .any(|window| window == b"/Encrypt"),
            "copy-encrypted linearization must carry the trailer encryption reference"
        );
    }

    #[test]
    fn canonical_linearized_copy_encryption_covers_cleartext_metadata_branch() {
        let mut pdf = open_cleartext_metadata_encrypted_three_page_pdf();
        let source = pdf
            .writer_copy_encryption_source()
            .expect("authenticated donor snapshot")
            .expect("encrypted fixture must provide copy parameters");
        let options = WriterOptions {
            object_streams: crate::writer::ObjectStreamMode::Generate,
            copy_encryption: Some(source),
            ..WriterOptions::default()
        };

        let (mut document, _) = write_linearized_for_pdf_writer(&mut pdf, &options, None)
            .expect("canonical cleartext-metadata copy-encryption write");
        document.back_patch().expect("back-patch final document");

        assert!(
            document
                .bytes
                .windows(b"/EncryptMetadata false".len())
                .any(|window| window == b"/EncryptMetadata false"),
            "copy-encrypted linearization must preserve /EncryptMetadata false"
        );
        assert!(
            document
                .bytes
                .windows(b"/Type /ObjStm".len())
                .any(|window| window == b"/Type /ObjStm"),
            "cleartext-metadata copy-encrypted linearization must contain ObjStm"
        );
    }

    #[test]
    fn canonical_linearized_copy_encryption_propagates_shape_errors() {
        let mut pdf = open_tiny_pdf();
        let options = WriterOptions {
            copy_encryption: Some(crate::encryption::CopyEncryptionSource {
                encrypt_dict: Dictionary::new(),
                file_key: Vec::new(),
                id0: Vec::new(),
                object_key_alg: crate::ObjectKeyAlg::Aes,
            }),
            ..WriterOptions::default()
        };

        let error = write_linearized_for_pdf_writer(&mut pdf, &options, None)
            .expect_err("invalid copy-encryption dictionary must fail");
        assert!(matches!(error, crate::Error::Unsupported(_)));
    }

    fn write_linearized_with_pass1_file_mode(
        object_streams: crate::writer::ObjectStreamMode,
    ) -> (Vec<u8>, LinearizedDocument) {
        let mut planning_pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(
            &mut planning_pdf,
            object_streams == crate::writer::ObjectStreamMode::Generate,
        )
        .expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let options = WriterOptions {
            object_streams,
            ..WriterOptions::default()
        };
        let temp = tempfile::tempdir().expect("temporary directory");
        let pass1_path = temp.path().join("pass1.pdf");
        let mut writing_pdf = open_tiny_pdf();
        let mut document = write_linearized_with_pass1_file(
            &plan,
            &renumber,
            &mut writing_pdf,
            &options,
            &pass1_path,
        )
        .expect("linearized write with pass-1 file");
        document.back_patch().expect("back-patch final document");
        let pass1 = std::fs::read(pass1_path).expect("read pass-1 file");
        (pass1, document)
    }

    fn pass1_comment_value(pass1: &[u8], key: &[u8]) -> usize {
        let value_start = pass1
            .windows(key.len())
            .rposition(|window| window == key)
            .map(|position| position + key.len())
            .expect("pass-1 comment must be present");
        let value_end = pass1[value_start..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map(|position| value_start + position)
            .expect("pass-1 comment must end with a newline");
        std::str::from_utf8(&pass1[value_start..value_end])
            .expect("pass-1 comment value must be UTF-8")
            .parse()
            .expect("pass-1 comment value must be decimal")
    }

    #[test]
    fn write_linearized_with_pass1_file_writes_classic_pass1() {
        let (pass1, document) =
            write_linearized_with_pass1_file_mode(crate::writer::ObjectStreamMode::Disable);

        assert!(pass1.starts_with(b"%PDF-"));
        assert_ne!(pass1, document.bytes);
        assert!(pass1
            .windows(b"% hint_offset=".len())
            .any(|w| w == b"% hint_offset="));
        assert!(pass1
            .windows(b"% hint_length=".len())
            .any(|w| w == b"% hint_length="));
        assert!(pass1.ends_with(b"% second_xref_end=0\n"));
    }

    #[test]
    fn write_linearized_with_pass1_file_records_xref_stream_end() {
        let (pass1, document) =
            write_linearized_with_pass1_file_mode(crate::writer::ObjectStreamMode::Generate);

        assert!(pass1.starts_with(b"%PDF-"));
        assert_ne!(pass1, document.bytes);
        assert!(pass1
            .windows(b"% hint_offset=".len())
            .any(|w| w == b"% hint_offset="));
        assert!(pass1
            .windows(b"% hint_length=".len())
            .any(|w| w == b"% hint_length="));
        assert!(pass1_comment_value(&pass1, b"% second_xref_end=") > 0);
    }

    #[test]
    fn write_linearized_with_pass1_file_preserves_open_error_context() {
        let mut planning_pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut planning_pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let temp = tempfile::tempdir().expect("temporary directory");
        let pass1_path = temp.path().join("missing-parent").join("pass1.pdf");
        let mut writing_pdf = open_tiny_pdf();

        let error = write_linearized_with_pass1_file(
            &plan,
            &renumber,
            &mut writing_pdf,
            &WriterOptions::default(),
            &pass1_path,
        )
        .expect_err("missing pass-1 parent must fail before returning final output");

        match &error {
            crate::Error::FileIo {
                operation,
                path,
                source,
            } => {
                assert_eq!(*operation, "open");
                assert_eq!(path, &pass1_path);
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected file-aware pass-1 error, got {other:?}"), // cov:ignore: assertion failure arm
        }
        assert_eq!(
            std::error::Error::source(&error)
                .expect("file-aware error must retain source")
                .downcast_ref::<std::io::Error>()
                .expect("source must be io::Error")
                .kind(),
            std::io::ErrorKind::NotFound
        );
        let diagnostic = error.to_string();
        assert!(diagnostic.starts_with(&format!("open {}: ", pass1_path.display())));
        assert!(diagnostic.contains("No such file") || diagnostic.contains("cannot find"));
    }

    #[test]
    fn pass1_stdio_finish_ignores_non_ebadf_and_maps_ebadf_to_internal_error() {
        let path = Path::new("pass1.pdf");
        let body = vec![b'x'; 4095];
        let mut enospc = FinishErrorWriter {
            errno: 28,
            bytes: Vec::new(),
        };
        write_pass1_stdio_body(&mut enospc, &body, path)
            .expect("qpdf ignores non-EBADF stdio finish failures");
        assert_eq!(enospc.bytes, body);

        let mut ebadf = FinishErrorWriter {
            errno: 9,
            bytes: Vec::new(),
        };
        let error = write_pass1_stdio_body(&mut ebadf, &body, path)
            .expect_err("qpdf maps EBADF during stdio finish to a logic error");
        assert!(matches!(
            error,
            crate::Error::Internal(ref message)
                if message == "linearization pass1: Pl_StdioFile::finish: stream already closed"
        ));
    }

    #[test]
    fn pass1_stdio_direct_zero_progress_maps_to_file_aware_write_error() {
        let path = Path::new("pass1.pdf");
        let mut writer = ZeroWriter;
        let error = write_pass1_stdio_body(&mut writer, &[b'x'; 4096], path)
            .expect_err("qpdf treats zero fwrite progress as a direct write failure");

        match error {
            crate::Error::FileIo {
                operation,
                path: error_path,
                source,
            } => {
                assert_eq!(operation, "write");
                assert_eq!(error_path, path);
                assert_eq!(source.kind(), std::io::ErrorKind::WriteZero);
            }
            other => panic!("expected file-aware zero-progress error, got {other:?}"), // cov:ignore: assertion failure arm
        }
    }

    #[cfg(unix)]
    #[test]
    fn write_linearized_with_pass1_file_ignores_small_body_finish_enospc() {
        let pass1_path = Path::new("/dev/full");
        if !pass1_path.exists() {
            // cov:ignore-start: environment-specific skip for Unix systems without /dev/full
            eprintln!("skipping: /dev/full is unavailable");
            return;
            // cov:ignore-end
        }
        let mut planning_pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut planning_pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let mut writing_pdf = open_tiny_pdf();

        let document = write_linearized_with_pass1_file(
            &plan,
            &renumber,
            &mut writing_pdf,
            &WriterOptions::default(),
            pass1_path,
        )
        .expect("qpdf ignores non-EBADF failure while finishing a buffered small pass-1 body");

        assert!(document.bytes.starts_with(b"%PDF-"));
    }

    #[cfg(unix)]
    #[test]
    fn write_linearized_with_pass1_file_propagates_large_body_direct_enospc() {
        let pass1_path = Path::new("/dev/full");
        if !pass1_path.exists() {
            // cov:ignore-start: environment-specific skip for Unix systems without /dev/full
            eprintln!("skipping: /dev/full is unavailable");
            return;
            // cov:ignore-end
        }
        let input_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat/objstm-lin-outlines-80-200.pdf");
        let planning_file = std::fs::File::open(&input_path).expect("open large fixture for plan");
        let mut planning_pdf = Pdf::open(std::io::BufReader::new(planning_file))
            .expect("large fixture parses for plan");
        let options = WriterOptions::default();
        let plan = LinearizationPlan::from_pdf_with_object_stream_mode(
            &mut planning_pdf,
            options.object_streams,
        )
        .expect("large fixture plan");
        let renumber = RenumberMap::from_plan(&plan);
        let writing_file = std::fs::File::open(&input_path).expect("open large fixture for write");
        let mut writing_pdf = Pdf::open(std::io::BufReader::new(writing_file))
            .expect("large fixture parses for write");

        let error = write_linearized_with_pass1_file(
            &plan,
            &renumber,
            &mut writing_pdf,
            &options,
            pass1_path,
        )
        .expect_err("large pass-1 body must surface its direct write failure");

        match &error {
            crate::Error::FileIo {
                operation,
                path,
                source,
            } => {
                assert_eq!(*operation, "write");
                assert_eq!(path, pass1_path);
                assert_ne!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected file-aware pass-1 write error, got {other:?}"), // cov:ignore: assertion failure arm
        }
    }

    #[test]
    fn validate_per_page_private_objects_rejects_mismatched_page_hints() {
        let plan = LinearizationPlan {
            page_hints: vec![crate::linearization::plan::PageHintEntry::placeholder(
                ObjectRef::new(3, 0),
            )],
            per_page_private_objects: Vec::new(),
            ..Default::default()
        };

        let error = validate_per_page_private_objects(&plan)
            .expect_err("a page/private-object length mismatch must be rejected");
        assert!(matches!(
            error,
            crate::Error::Unsupported(ref message)
                if message.contains("per_page_private_objects length (0) does not")
                    && message.contains("page_hints length (1)")
        ));
    }

    #[test]
    fn separate_write_handle_indirectizes_direct_outlines_like_plan_handle() {
        let bytes = direct_outlines_pdf_bytes();
        let mut planning_pdf = Pdf::open(Cursor::new(bytes.clone())).expect("fixture parses");
        let plan = LinearizationPlan::from_pdf(&mut planning_pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);

        let mut writing_pdf = Pdf::open(Cursor::new(bytes)).expect("write fixture parses");
        let options = WriterOptions {
            object_streams: crate::writer::ObjectStreamMode::Disable,
            ..WriterOptions::default()
        };
        let mut document = write_linearized(&plan, &renumber, &mut writing_pdf, &options)
            .expect("linearized write");
        document.back_patch().expect("back-patch");

        let mut output =
            Pdf::open(Cursor::new(document.bytes)).expect("linearized output should parse");
        let root_ref = output.root_ref().expect("output has /Root");
        let root = output
            .resolve_object(root_ref)
            .expect("output catalog resolves");
        let root = root.into_dict().expect("output root must be a dictionary");
        assert!(matches!(root.get("Outlines"), Some(Object::Reference(_))));
    }

    #[test]
    fn suppressed_object_stream_plan_rebuild_propagates_missing_root() {
        let mut planning_pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut planning_pdf, true).expect("valid plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            force_version: Some("1.4".to_string()),
            object_streams: crate::writer::ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let mut write_pdf = pdf_without_root();

        let err = write_linearized(&plan, &renumber, &mut write_pdf, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref message)
                if message == "reachability: trailer has no /Root"),
            "suppressed plan rebuild must propagate its missing-root error; got {err:?}"
        );
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
    // write_linearized re-runs the optimization preparation prefix on its own
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
    // pages::repair::tests::excessive_depth_returns_unsupported_error.
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

        // Deep write-handle. WriterOptions::default() leaves deterministic_id /
        // static_id false and encrypt / copy_encryption None, so the option
        // guards ahead of the push are no-ops and the push is the first fallible
        // step reached.
        let mut deep_pdf =
            Pdf::open(Cursor::new(deep_pages_pdf_bytes())).expect("deep fixture parses");

        let result = write_linearized(&plan, &renumber, &mut deep_pdf, &WriterOptions::default());
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
        let doc = write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default())
            .expect("write");

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
        assert!(
            !plan.part2_objects.contains(&ObjectRef::new(1, 0)),
            "qpdf's is_root precedence must keep the catalog out of part2"
        );
        assert!(
            plan.part4_open_document_plain
                .contains(&ObjectRef::new(1, 0)),
            "the qpdf root must remain in the first half before /O"
        );
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 =
            Pdf::open(Cursor::new(catalog_backref_pdf_bytes())).expect("backref PDF must parse");
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default())
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

    /// Three pages + a `/Outlines` root, built so the writer's plan populates
    /// BOTH hint tables the reviewer-flagged bug named: page 0 has its own
    /// private font (5 0 R, referenced only by page 0, so it stays a
    /// first-page-private object) while pages 1 and 2 share a DIFFERENT font
    /// (9 0 R) that page 0 never references — per qpdf's classification, a
    /// shared object is Part 3 (first half) only when page 0 is among its
    /// referencing pages; shared *without* page 0 among the referencers goes
    /// to Part 8 (`part4_other_pages_shared`, second half), which is what
    /// populates `so_table.header.location`. The empty `/Outlines` dict gives
    /// `compute_outline_hint_info` a non-empty retained outline set, which
    /// populates the Outlines Hint Table's `first_object_offset`.
    fn outlines_and_part8_shared_pdf_bytes() -> Vec<u8> {
        outlines_and_part8_shared_pdf_bytes_with_payload(
            b"BT /F1 12 Tf 72 700 Td (hi) Tj ET\n",
            b"outline-producer-printable",
        )
    }

    /// Variant of [`outlines_and_part8_shared_pdf_bytes`] for deterministic
    /// encryption tests. Keeping the graph identical while varying the body
    /// payload lets the tests select ciphertext bytes on either side of the
    /// hint-stream framing boundary without changing which hint tables exist.
    fn outlines_and_part8_shared_pdf_bytes_with_payload(
        content: &[u8],
        producer: &[u8],
    ) -> Vec<u8> {
        outlines_and_part8_shared_pdf_bytes_with_payload_and_id(content, producer, None)
    }

    /// As [`outlines_and_part8_shared_pdf_bytes_with_payload`], with an
    /// optional fixed input `/ID[0]` for deterministic encryption-key variants.
    fn outlines_and_part8_shared_pdf_bytes_with_payload_and_id(
        content: &[u8],
        producer: &[u8],
        id0: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut pdf: Vec<u8> = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = [0usize; 12];

        offs[1] = pdf.len();
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R \
              /Outlines << /Type /Outlines /Count 0 >> >>\nendobj\n",
        );
        offs[2] = pdf.len();
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 6 0 R 10 0 R] /Count 3 >>\nendobj\n",
        );

        // Page 0: private font 5 0 R, not shared with any other page.
        offs[3] = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
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

        // Page 1: shares font 9 0 R with page 2 (below), never page 0.
        offs[6] = pdf.len();
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 7 0 R /Resources << /Font << /F2 9 0 R >> >> >>\nendobj\n",
        );
        offs[7] = pdf.len();
        pdf.extend_from_slice(
            format!("7 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        offs[9] = pdf.len();
        pdf.extend_from_slice(
            b"9 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>\nendobj\n",
        );

        // Page 2: also references the shared font 9 0 R.
        offs[10] = pdf.len();
        pdf.extend_from_slice(
            b"10 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 8 0 R /Resources << /Font << /F2 9 0 R >> >> >>\nendobj\n",
        );
        offs[8] = pdf.len();
        pdf.extend_from_slice(
            format!("8 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Nested printable body strings make this same Part-8/Outlines
        // fixture exercise AES wire syntax on every randomized write pass.
        offs[11] = pdf.len();
        pdf.extend_from_slice(
            b"11 0 obj\n<< /Nested [(array-printable) << /Inner (inner-printable) >>] \
              /Producer (",
        );
        pdf.extend_from_slice(producer);
        pdf.extend_from_slice(b") >>\nendobj\n");

        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 12\n0000000000 65535 f \n");
        for off in offs.iter().skip(1) {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        let id_entry = id0
            .filter(|bytes| !bytes.is_empty())
            .map(|bytes| {
                let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                format!(" /ID [<{hex}><{hex}>]")
            })
            .unwrap_or_default();
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size 12 /Root 1 0 R /Info 11 0 R{id_entry} >>\n\
                 startxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn outlines_and_part8_fixture_can_emit_supplied_source_id() {
        let source = outlines_and_part8_shared_pdf_bytes_with_payload_and_id(
            b"BT /F1 12 Tf 72 700 Td (id fixture) Tj ET\n",
            b"outline-producer-id",
            Some(&[146_u8; 16]),
        );
        let expected_id =
            b"/ID [<92929292929292929292929292929292><92929292929292929292929292929292>]";
        assert!(
            source
                .windows(expected_id.len())
                .any(|window| window == expected_id),
            "fixture must include the supplied source ID"
        );
    }

    #[test]
    fn shared_catalog_remains_qpdf_part4_and_is_emitted_once() {
        let mut pdf = Pdf::open(Cursor::new(catalog_backref_two_page_pdf_bytes()))
            .expect("two-page backref PDF must parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        assert!(
            !plan.part3_objects.contains(&ObjectRef::new(1, 0)),
            "qpdf's is_root precedence must keep the catalog out of part3"
        );
        assert!(
            plan.part4_open_document_plain
                .contains(&ObjectRef::new(1, 0)),
            "the qpdf root must remain in the first half before /O"
        );
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(catalog_backref_two_page_pdf_bytes()))
            .expect("two-page backref PDF must parse");
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default())
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

    #[test]
    fn two_page_shared_resource_output_has_no_qpdf_object_count_warning() {
        let mut pdf = Pdf::open(Cursor::new(catalog_backref_two_page_pdf_bytes()))
            .expect("two-page shared-resource PDF must parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = Pdf::open(Cursor::new(catalog_backref_two_page_pdf_bytes()))
            .expect("two-page shared-resource PDF must parse");
        let mut doc = write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default())
            .expect("write_linearized");
        doc.back_patch().expect("back_patch");

        let temp = tempfile::tempdir().expect("temporary directory");
        let path = temp.path().join("shared-resource.pdf");
        std::fs::write(&path, doc.bytes).expect("write linearized output");
        let output = std::process::Command::new("qpdf")
            .args(["--check-linearization", path.to_str().expect("UTF-8 path")])
            .output()
            .expect("qpdf 11.9.0 must be available for this oracle test");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "qpdf --check-linearization failed: {stderr}"
        );
        assert!(
            !stderr.contains("object count mismatch for page 0"),
            "qpdf reported a shared-resource hint mismatch:\n{stderr}"
        );
        assert!(
            !stderr.contains("in hint table but not computed list"),
            "qpdf reported a phantom shared-resource hint entry:\n{stderr}"
        );
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
        let opts = WriterOptions {
            deterministic_id: true,
            object_streams,
            ..WriterOptions::default()
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

    fn id_array_hex_parts(id: &[u8]) -> (&[u8], &[u8]) {
        let lt0 = id
            .iter()
            .position(|&byte| byte == b'<')
            .expect("id0 opening");
        let gt0 = id[lt0 + 1..]
            .iter()
            .position(|&byte| byte == b'>')
            .map(|offset| lt0 + 1 + offset)
            .expect("id0 closing");
        let lt1 = id[gt0 + 1..]
            .iter()
            .position(|&byte| byte == b'<')
            .map(|offset| gt0 + 1 + offset)
            .expect("id1 opening");
        let gt1 = id[lt1 + 1..]
            .iter()
            .position(|&byte| byte == b'>')
            .map(|offset| lt1 + 1 + offset)
            .expect("id1 closing");
        (&id[lt0 + 1..gt0], &id[lt1 + 1..gt1])
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

    fn linearize_with_pass1(
        source_bytes: &[u8],
        object_streams: crate::writer::ObjectStreamMode,
        options: WriterOptions,
    ) -> (Vec<u8>, Vec<u8>) {
        let use_generate = object_streams == crate::writer::ObjectStreamMode::Generate;
        let mut planning_pdf =
            Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses for planning");
        let plan = LinearizationPlan::from_pdf(&mut planning_pdf, use_generate).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let temp = tempfile::tempdir().expect("temporary directory");
        let pass1_path = temp.path().join("pass1.pdf");
        let mut writing_pdf =
            Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses for writing");
        let mut document = write_linearized_with_pass1_file(
            &plan,
            &renumber,
            &mut writing_pdf,
            &WriterOptions {
                object_streams,
                ..options
            },
            &pass1_path,
        )
        .expect("linearized write with pass-1 file");
        document.back_patch().expect("back-patch final document");
        let pass1 = std::fs::read(pass1_path).expect("read pass-1 file");
        (pass1, document.bytes)
    }

    /// qpdf 11.9.0 `QPDFWriter::writeTrailer` writes placeholders at every
    /// pass-1 `/ID` site regardless of the final ID policy. The first zero
    /// string keeps the original `/ID[0]` byte width and the second is always
    /// 16 bytes. The final pass still preserves the original permanent ID.
    #[test]
    fn pass1_ids_are_zero_placeholders_for_every_id_policy_and_xref_shape() {
        let source = tiny_pdf_with(
            "/ID [<aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa> \
             <bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb>]",
            None,
        );
        let expected_pass1 =
            b"[<0000000000000000000000000000000000000000><00000000000000000000000000000000>]";
        let expected_final_prefix = b"[<aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa><";

        for object_streams in [
            crate::writer::ObjectStreamMode::Disable,
            crate::writer::ObjectStreamMode::Generate,
        ] {
            for (policy, options) in [
                ("default", WriterOptions::default()),
                (
                    "static",
                    WriterOptions {
                        static_id: true,
                        ..WriterOptions::default()
                    },
                ),
                (
                    "deterministic",
                    WriterOptions {
                        deterministic_id: true,
                        ..WriterOptions::default()
                    },
                ),
            ] {
                let (pass1, final_bytes) = linearize_with_pass1(&source, object_streams, options);
                let pass1_ids = collect_id_arrays(&pass1);
                assert_eq!(
                    pass1_ids.len(),
                    2,
                    "{policy}/{object_streams:?}: pass 1 must carry two /ID sites"
                );
                assert!(
                    pass1_ids
                        .iter()
                        .all(|id| id.as_slice() == expected_pass1),
                    "{policy}/{object_streams:?}: every pass-1 /ID must be the qpdf-width zero placeholder: {pass1_ids:?}"
                );

                let final_ids = collect_id_arrays(&final_bytes);
                assert_eq!(
                    final_ids.len(),
                    2,
                    "{policy}/{object_streams:?}: final output must carry two /ID sites"
                );
                assert!(
                    final_ids.iter().all(|id| id == &final_ids[0]),
                    "{policy}/{object_streams:?}: final /ID sites must agree: {final_ids:?}"
                );
                assert!(
                    final_ids[0].starts_with(expected_final_prefix),
                    "{policy}/{object_streams:?}: final output must preserve source /ID[0]"
                );
                assert_ne!(
                    final_ids[0].as_slice(),
                    expected_pass1,
                    "{policy}/{object_streams:?}: pass-1 placeholder must not replace the final /ID"
                );
            }
        }
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

        // Default WriterOptions => compress_streams = Yes, stream_data = None.
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
        let filtered_hint: &[u8] = b"<< /Filter /FlateDecode /S ";
        assert!(
            out.windows(filtered_hint.len()).any(|w| w == filtered_hint),
            "compress mode must FlateDecode the primary hint stream"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("compress-mode linearized output must pass the checker");
        // The output reparses and the re-encoded content decodes back to the
        // original raw payload, proving recompression is lossless.
        let mut reopened = Pdf::open(Cursor::new(out.clone())).expect("output must reparse");
        let refs = reopened.live_object_refs();
        let decoded_any_match = refs.into_iter().any(|r| {
            reopened
                .resolve_object(r)
                .ok()
                .and_then(|o| o.into_stream())
                .and_then(|stream| {
                    crate::filters::test_dictionary_api::decode_stream_data(
                        &stream.dict,
                        &stream.data,
                    )
                    .ok()
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
    /// content streams: the source dict + raw payload pass through unchanged.
    /// This exercises [`append_body_object`] when [`effective_stream_policy`]
    /// yields `None` (the only non-recompressing branch on the linearized body
    /// path).
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

    /// Uncompress mode applies to qpdf's generated primary hint stream too:
    /// emit the bit-packed table directly and omit `/Filter`.
    #[test]
    fn linearized_uncompress_mode_emits_raw_hint_stream() {
        let src = tiny_pdf_with_placeholder_in_content();
        let out = linearize_with(&src, |o| {
            o.deterministic_id = true;
            o.stream_data = Some(crate::writer::StreamDataMode::Uncompress);
        });

        let raw_hint: &[u8] = b"<< /S ";
        assert!(
            out.windows(raw_hint.len()).any(|w| w == raw_hint),
            "uncompress mode must emit an unfiltered primary hint stream"
        );
        let filtered_hint: &[u8] = b"<< /Filter /FlateDecode /S ";
        assert!(
            !out.windows(filtered_hint.len()).any(|w| w == filtered_hint),
            "uncompress mode must omit /Filter from the primary hint stream"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("uncompress-mode linearized output must pass the checker");
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
            crate::filters::test_dictionary_api::encode_stream_data(&enc_dict, payload)
                .expect("flate encode");

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

        // Default WriterOptions => compress_streams = Yes, recompress_flate =
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
                let stream = reopened.resolve_object(r).ok()?.into_stream()?;
                let decoded = crate::filters::test_dictionary_api::decode_stream_data(
                    &stream.dict,
                    &stream.data,
                )
                .ok()?;
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
            crate::writer::is_lone_flate(stream.dict.get("Filter")),
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
    fn linearized_id_uses_live_trailer_after_merge_mutation() {
        let original_id = "0102030405060708090a0b0c0d0e0f10";
        let merged_id = "202122232425262728292a2b2c2d2e2f";
        let source = tiny_pdf_with(
            &format!("/ID [<{original_id}> <ffffffffffffffffffffffffffffffff>]"),
            None,
        );
        let mut pdf = Pdf::open(Cursor::new(source)).expect("source parses");

        // page_merge::wire_primary_trailer mutates the target's live trailer
        // handle. The construction-time dictionary must not remain the ID
        // source for the later linearized writer.
        pdf.trailer()
            .replace_key(
                b"/ID",
                ObjectHandle::array(vec![
                    ObjectHandle::string(hex::decode(merged_id).unwrap()),
                    ObjectHandle::string(vec![0xff; 16]),
                ]),
            )
            .expect("live trailer mutation");

        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let options = WriterOptions {
            static_id: true,
            ..WriterOptions::default()
        };
        let mut document =
            write_linearized(&plan, &renumber, &mut pdf, &options).expect("linearized output");
        document.back_patch().expect("back patch");

        let id = first_id_array(&document.bytes);
        assert_eq!(
            &id[ID0_HEX],
            merged_id.as_bytes(),
            "linearized /ID[0] must come from the live trailer after merge"
        );
        assert_ne!(
            &id[ID0_HEX],
            original_id.as_bytes(),
            "linearized /ID[0] must not read the stale construction snapshot"
        );
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
        let doc = write_linearized(&plan, &renumber, &mut pdf2, &WriterOptions::default())
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

    /// Linearize `source_bytes` in the given write mode with the supplied
    /// `WriterOptions` mutator applied, returning the fully back-patched bytes.
    /// Mirrors [`linearize_deterministic_mode`] but lets a test pick a
    /// non-deterministic `/ID` policy (e.g. `--static-id`).
    fn linearize_with(source_bytes: &[u8], configure: impl FnOnce(&mut WriterOptions)) -> Vec<u8> {
        let mut pdf = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let mut opts = WriterOptions::default();
        configure(&mut opts);
        let mut pdf2 = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let mut doc =
            write_linearized(&plan, &renumber, &mut pdf2, &opts).expect("write_linearized");
        doc.back_patch().expect("back_patch must succeed");
        doc.bytes
    }

    /// Configure the deterministic AES-128 mode used by the two framing
    /// boundary fixtures. `static_id` fixes the encryption key inputs while
    /// `static_aes_iv` fixes every AES IV; neither setting is used by the
    /// random-IV production-path regression below.
    fn configure_deterministic_aes128(options: &mut WriterOptions) {
        options.static_id = true;
        options.static_aes_iv = true;
        options.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
            Vec::new(),
            b"owner".to_vec(),
        ));
    }

    // These fixed IVs were selected against the complete three-page fixture
    // below. They produce opposite ciphertext-last-byte framing outcomes while
    // leaving the hint tables and the encryption key inputs unchanged. The
    // compatibility feature uses a different compression implementation, so it
    // needs its own pair of IVs to reach the same framing boundaries.
    #[cfg(not(feature = "qpdf-zlib-compat"))]
    const HINT_IV_NO_FRAMING_NEWLINE: [u8; 16] = [
        172, 10, 255, 113, 144, 81, 27, 235, 20, 47, 87, 178, 60, 65, 226, 247,
    ];
    #[cfg(not(feature = "qpdf-zlib-compat"))]
    const HINT_IV_WITH_FRAMING_NEWLINE: [u8; 16] = [
        144, 3, 147, 208, 28, 39, 53, 128, 99, 2, 45, 208, 190, 187, 8, 27,
    ];
    #[cfg(feature = "qpdf-zlib-compat")]
    const HINT_IV_NO_FRAMING_NEWLINE: [u8; 16] = [
        173, 114, 118, 125, 12, 37, 246, 18, 191, 188, 47, 115, 60, 50, 129, 185,
    ];
    #[cfg(feature = "qpdf-zlib-compat")]
    const HINT_IV_WITH_FRAMING_NEWLINE: [u8; 16] = [
        174, 226, 85, 64, 141, 180, 169, 76, 244, 19, 246, 109, 168, 31, 180, 103,
    ];

    /// Compute the encrypted output's hint-stream object number from the same
    /// plan construction as [`linearize_with`]. Encryption reserves one slot
    /// immediately before the hint stream, so the final number is the
    /// unencrypted hint slot plus one.
    fn encrypted_hint_stream_number(source_bytes: &[u8]) -> u32 {
        let mut pdf = Pdf::open(Cursor::new(source_bytes.to_vec())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        RenumberMap::from_plan(&plan).hint_stream_slot() + 1
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
            .trailer_dictionary()
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
            reopened.trailer_dictionary().get("ID").is_some(),
            "static-id linearized output must carry /ID in the reader-visible main trailer"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("static-id linearized output must pass the linearization checker");
    }

    #[test]
    fn ordinary_linearized_empty_source_id0_matches_qpdf_default_and_static() {
        for id_entry in ["/ID [<> <bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb>]", "/ID [() ()]"] {
            let source = tiny_pdf_with(id_entry, None);

            let default_output = linearize_with(&source, |_| {});
            let default_ids = collect_id_arrays(&default_output);
            assert_eq!(default_ids.len(), 2);
            assert!(default_ids.iter().all(|id| id == &default_ids[0]));
            let (default_id0, default_id1) = id_array_hex_parts(&default_ids[0]);
            assert!(!default_id0.is_empty());
            assert_eq!(default_id0, default_id1);
            crate::linearization::check_linearization_bytes(&default_output)
                .expect("default output must pass the linearization checker");

            let static_output = linearize_with(&source, |options| options.static_id = true);
            let static_ids = collect_id_arrays(&static_output);
            assert_eq!(static_ids.len(), 2);
            assert!(static_ids.iter().all(|id| id == &static_ids[0]));
            let (static_id0, static_id1) = id_array_hex_parts(&static_ids[0]);
            let pi_hex = b"31415926535897932384626433832795";
            assert_eq!(static_id0, pi_hex);
            assert_eq!(static_id1, pi_hex);
            crate::linearization::check_linearization_bytes(&static_output)
                .expect("static output must pass the linearization checker");
        }
    }

    #[test]
    fn encrypted_linearized_empty_source_id0_matches_emitted_id1() {
        let source = tiny_pdf_with("/ID [<> <bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb>]", None);
        let output = linearize_with(&source, |options| {
            options.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
                Vec::new(),
                b"owner".to_vec(),
            ));
        });
        let ids = collect_id_arrays(&output);
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| id == &ids[0]));
        let (id0, id1) = id_array_hex_parts(&ids[0]);
        assert!(!id0.is_empty());
        assert_eq!(
            id0, id1,
            "empty source /ID[0] must reuse qpdf's generated id2"
        );

        crate::linearization::check_linearization_bytes(&output)
            .expect("encrypted output must pass the linearization checker");
        let reopened =
            Pdf::open_with_options(Cursor::new(output), crate::PdfOpenOptions::default())
                .expect("encrypted output must reopen with the empty user password");
        assert!(reopened.trailer_dictionary().get("Encrypt").is_some());
    }

    /// `--deterministic-id` combined with encryption reaches qpdf's
    /// `QPDFWriter::generateID` logic error before bytes are emitted: a
    /// content-derived `/ID` cannot be computed once the bytes are encrypted,
    /// and the file encryption key itself derives from `/ID[0]`
    /// (PDF Algorithm 2). This does not mean linearize+encrypt is unsupported
    /// in general —
    /// non-deterministic (default) and `--static-id` `/ID`s combine with
    /// encryption just fine; see [`write_linearized`]'s `# Errors` section.
    /// The returned [`crate::Error::Internal`] carries qpdf's exact
    /// `generateID` message.
    #[test]
    fn deterministic_id_linearized_rejects_encrypt() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            deterministic_id: true,
            encrypt: Some(crate::encryption::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(
                err,
                crate::Error::Internal(ref message)
                    if message == "INTERNAL ERROR: QPDFWriter::generateID has no data for deterministic ID.  This may happen if deterministic ID and file encryption are requested together."
            ),
            "got {err:?}"
        );
    }

    /// `--deterministic-id` and `--static-id` are mutually exclusive on the
    /// linearized write path too, mirroring `emit_canonical_pdf`. Without
    /// the guard the deterministic branch silently wins over `static_id`.
    #[test]
    fn deterministic_id_linearized_rejects_static_id() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            deterministic_id: true,
            static_id: true,
            ..WriterOptions::default()
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
        let opts = WriterOptions {
            deterministic_id: true,
            copy_encryption: Some(crate::encryption::CopyEncryptionSource {
                encrypt_dict: Dictionary::new(),
                file_key: Vec::new(),
                id0: Vec::new(),
                object_key_alg: crate::ObjectKeyAlg::Aes,
            }),
            ..WriterOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(
                err,
                crate::Error::Internal(ref message)
                    if message == "INTERNAL ERROR: QPDFWriter::generateID has no data for deterministic ID.  This may happen if deterministic ID and file encryption are requested together."
            ),
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
        let opts = WriterOptions {
            deterministic_id: true,
            ..WriterOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        write_linearized(&plan, &renumber, &mut pdf2, &opts)
            .expect("deterministic-id without encryption must succeed");
    }

    /// The `deterministic_id && encrypting` guard is scoped to exactly that
    /// combination (mirroring qpdf's real restriction, see the guard's own
    /// comment). A *non*-deterministic-id encrypting request must not be
    /// rejected by it — qpdf itself supports `--linearize --encrypt` without
    /// `--deterministic-id` (verified empirically against qpdf 11.9.0).
    ///
    /// The write now succeeds outright (the `/Encrypt` object is emitted at
    /// its reserved slot — see
    /// [`linearize_with_encrypt_emits_encrypt_dict_at_reserved_object_number`]
    /// for the dedicated object-placement assertion), so this asserts full
    /// success directly rather than merely excluding the deterministic-id
    /// guard's message from an otherwise-unconstrained `Result`.
    #[test]
    fn non_deterministic_encrypt_linearize_no_longer_rejected_by_guard() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            // `deterministic_id` left at its default `false`.
            encrypt: Some(crate::encryption::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        write_linearized(&plan, &renumber, &mut pdf2, &opts)
            .expect("non-deterministic-id encrypting must not hit the deterministic-id guard");
    }

    /// `--cleartext-metadata` (`encrypt_metadata: false`) calls
    /// `crate::writer::resolve_metadata_stream_ref` when building the
    /// `EncryptionContext`, mirroring the full-rewrite writer's own
    /// `--cleartext-metadata` gating (`!params.encrypt_metadata`).
    /// `tiny_pdf_bytes()`'s catalog has no `/Metadata` entry, so this only
    /// pins that setting the option doesn't change *whether* the write
    /// succeeds (same as
    /// [`non_deterministic_encrypt_linearize_no_longer_rejected_by_guard`],
    /// now that the `/Encrypt` object is actually emitted — see
    /// [`linearize_with_encrypt_emits_encrypt_dict_at_reserved_object_number`]).
    /// The `Some(metadata_ref)` exemption path's actual effect (leaving the
    /// `/Metadata` stream in the clear while every other body value is still
    /// encrypted) is pinned by
    /// [`linearize_with_encrypt_cleartext_metadata_exempts_only_metadata_stream`],
    /// which uses a `/Metadata`-bearing fixture.
    #[test]
    fn non_deterministic_encrypt_linearize_cleartext_metadata_option_reaches_same_point() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            encrypt: Some(crate::encryption::EncryptParams {
                encrypt_metadata: false,
                ..crate::encryption::EncryptParams::v4_aes128(b"user".to_vec(), b"owner".to_vec())
            }),
            ..WriterOptions::default()
        };
        let mut pdf2 = open_tiny_pdf();
        write_linearized(&plan, &renumber, &mut pdf2, &opts)
            .expect("cleartext-metadata must not change whether the write succeeds");
    }

    /// Building the `/Encrypt` object's `EncryptionContext`, reserving its
    /// object slot, and emitting its bytes for linearized output
    /// (non-deterministic `/ID`, no object streams). Before this task the
    /// slot was reserved but never emitted, so `write_linearized` failed its
    /// own Part-1 xref consistency check (`crate::Error::Unsupported("Part-1
    /// xref: covered object N has no offset …")`, see
    /// [`non_deterministic_encrypt_linearize_no_longer_rejected_by_guard`]'s
    /// doc for the pre-emission history). Now that the object is emitted at
    /// its reserved slot, `write_linearized` succeeds outright: this test
    /// pins BOTH that the write no longer errors AND that the reserved
    /// object number (`renumber.hint_stream_slot()` before reservation —
    /// see [`RenumberMap::reserve_encrypt_dict_slot`]'s doc for why that
    /// equals the assigned `/Encrypt` object number for this ObjStm-free
    /// fixture) carries the `/Encrypt` dictionary specifically, not merely
    /// that `/Filter /Standard` appears somewhere in the output. It also
    /// pins the trailer wiring: the first-page (Part-1) trailer carries
    /// `/Encrypt {N} 0 R` right after `/ID` (qpdf `writeTrailer`,
    /// QPDFWriter.cc:1224-1231 — the reference is written for every trailer
    /// form except `t_lin_second`, the main/second-half trailer), and the
    /// main trailer at EOF carries no `/Encrypt` at all — checked both by
    /// scanning the main trailer's own bytes and by confirming `/Encrypt`
    /// appears exactly once across the whole output. Verified empirically
    /// against qpdf 11.9.0 (`qpdf --linearize --static-id --static-aes-iv
    /// --encrypt "" "" 128 --use-aes=y`), which produces the same `/ID [...]
    /// /Encrypt N 0 R >>` sequence in its first-page trailer. Per-object
    /// string/stream encryption (Task 8) remains separate follow-up work.
    #[test]
    fn linearize_with_encrypt_emits_encrypt_dict_at_reserved_object_number() {
        // Independently recompute the object number `write_linearized` will
        // assign to `/Encrypt`, from the same plan/renumber construction
        // `linearize_with` performs internally on the same source bytes.
        // `RenumberMap::reserve_encrypt_dict_slot` (invoked inside
        // `write_linearized` on its own clone of an equivalent map) always
        // returns `ObjectRef::new(hint_stream_slot(), 0)` — reading
        // `hint_stream_slot()` here, before any reservation, gives the same
        // number without duplicating the reservation call itself. No ObjStm
        // relocation runs for this fixture (`tiny_pdf_bytes()` carries no
        // source ObjStm and the default `ObjectStreamMode` is a no-op on an
        // ObjStm-free source), so nothing shifts `hint_stream_slot()` between
        // this computation and `write_linearized`'s internal one.
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let expected_encrypt_num = renumber.hint_stream_slot();

        let out = linearize_with(&tiny_pdf_bytes(), |o| {
            o.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
                b"user".to_vec(),
                b"owner".to_vec(),
            ));
        });

        // Locate "<N> 0 obj" for the reserved number, then confirm
        // /Filter /Standard (the /Encrypt dict qpdf always includes) appears
        // inside THAT object's body (before its own "endobj"), not just
        // somewhere in the file. Every object header in this writer is
        // preceded by a newline (`append_object` emits "\nendobj\n" for the
        // prior object, or a trailer/xref section ends in "\n"), so search
        // for "\n<N> 0 obj" rather than "<N> 0 obj" alone — an unprefixed
        // search for e.g. "4 0 obj" would also match inside "14 0 obj" /
        // "24 0 obj" for larger fixtures.
        let header = format!("\n{expected_encrypt_num} 0 obj");
        let header_pos = out
            .windows(header.len())
            .position(|w| w == header.as_bytes())
            .unwrap_or_else(|| {
                // cov:ignore-start: only reached if the assertion below would
                // fail — the object header is always found once Task 6's
                // emission is correct, which is the state under test.
                panic!("expected \"{header}\" (the reserved /Encrypt object header) in output")
                // cov:ignore-end
            })
            + 1;
        let body = &out[header_pos..];
        let body_end = body
            .windows(b"endobj".len())
            .position(|w| w == b"endobj")
            .unwrap_or(body.len());
        let object_bytes = &body[..body_end];
        let needle: &[u8] = b"/Filter /Standard";
        assert!(
            object_bytes.windows(needle.len()).any(|w| w == needle),
            "object {expected_encrypt_num} (the reserved /Encrypt slot) must be the \
             /Encrypt dictionary (/Filter /Standard), got {:?}",
            String::from_utf8_lossy(object_bytes) // cov:ignore: only evaluated when the assertion above fails.
        );

        // Physical placement, not just a number coincidence:
        // `reserve_encrypt_dict_slot` inserts the `/Encrypt` slot at the OLD
        // `hint_stream_slot` and shifts the hint stream to `old + 1` (see its
        // own doc and `reserve_encrypt_dict_slot_inserts_before_hint_and_shifts_it`
        // in renumber.rs), so the hint stream's new object number is
        // `expected_encrypt_num + 1` — confirm ITS header appears physically
        // after the `/Encrypt` object's header, pinning the qpdf insertion
        // point (right after Part-4/open-document objects, before the hint
        // stream — QPDFWriter.cc:2793-2796) in file byte order, not just
        // object-number order.
        let hint_header = format!("\n{} 0 obj", expected_encrypt_num + 1);
        let hint_header_pos = out
            .windows(hint_header.len())
            .position(|w| w == hint_header.as_bytes())
            .unwrap_or_else(|| {
                // cov:ignore-start: only reached if the assertion below would
                // fail — the hint stream header is always found once Task 6's
                // emission is correct, which is the state under test.
                panic!("expected hint stream header \"{hint_header}\" in output")
                // cov:ignore-end
            })
            + 1;
        assert!(
            hint_header_pos > header_pos,
            "hint stream object {} must appear physically after the /Encrypt \
             object {expected_encrypt_num} in the output",
            expected_encrypt_num + 1 // cov:ignore: only evaluated when the assertion above fails.
        );

        // Trailer wiring (Task 7): the classic path emits two `trailer <<`
        // sections — the Part-1 first-page trailer, then the main (Part-6)
        // trailer at EOF. `/Encrypt` belongs in the first only.
        let trailer_needle: &[u8] = b"trailer << ";
        let first_trailer_pos = out
            .windows(trailer_needle.len())
            .position(|w| w == trailer_needle)
            .expect("classic linearized output must have a Part-1 trailer");
        let second_trailer_pos = out[first_trailer_pos + trailer_needle.len()..]
            .windows(trailer_needle.len())
            .position(|w| w == trailer_needle)
            .map(|p| p + first_trailer_pos + trailer_needle.len())
            .expect("classic linearized output must have a main (Part-6) trailer");

        let first_trailer = &out[first_trailer_pos..second_trailer_pos];
        let main_trailer = &out[second_trailer_pos..];

        // qpdf's key order and exact spacing is `/ID` then `/Encrypt`, right
        // before the dict's closing `>>`: `/ID` is written first, then — for
        // every `which != t_lin_second` trailer form — ` /Encrypt {objid} 0
        // R` (QPDFWriter.cc:1224-1231); the dict close follows
        // unconditionally. Pin the exact byte run observed against qpdf
        // 11.9.0 (`] /Encrypt 4 0 R >>`, this test's own oracle sample)
        // rather than just "contains /Encrypt somewhere after /ID": a
        // substring-only check can't distinguish qpdf's spacing from e.g.
        // `]/Encrypt N 0 R>>` or extra spaces, which would still be a byte
        // divergence in this qpdf-byte-identical project.
        let tail = format!("] /Encrypt {expected_encrypt_num} 0 R >>");
        assert!(
            first_trailer
                .windows(tail.len())
                .any(|w| w == tail.as_bytes()),
            "Part-1 trailer must end with qpdf's exact byte run \"{tail}\", got {:?}",
            String::from_utf8_lossy(first_trailer) // cov:ignore: only evaluated when the assertion above fails.
        );

        assert!(
            !main_trailer
                .windows(b"/Encrypt".len())
                .any(|w| w == b"/Encrypt"),
            "main (Part-6) trailer must NOT contain /Encrypt (qpdf omits it for \
             t_lin_second), got {:?}",
            String::from_utf8_lossy(main_trailer) // cov:ignore: only evaluated when the assertion above fails.
        );

        let total_encrypt_occurrences = out
            .windows(b"/Encrypt".len())
            .filter(|w| *w == b"/Encrypt")
            .count();
        assert_eq!(
            total_encrypt_occurrences, 1,
            "/Encrypt must appear exactly once in the whole output (the Part-1 \
             trailer reference); the /Encrypt dictionary object itself never \
             contains the literal key /Encrypt"
        );
    }

    /// Build a linearizable single-page PDF with a page content stream (raw
    /// `content` bytes, no `/Filter`) and an `/Info` dictionary carrying a
    /// `/Producer` string plus nested printable strings — the distinct kinds
    /// of body data the encrypted writer must handle: a stream payload, a
    /// printable stream-dictionary string, and strings at every container
    /// depth in an ordinary dictionary.
    fn tiny_pdf_with_content_and_producer(content: &[u8], producer: &[u8]) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 4 0 R >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"4 0 obj\n<< /Label (");
        pdf.extend_from_slice(STREAM_DICTIONARY_LABEL);
        pdf.extend_from_slice(format!(") /Length {} >>\nstream\n", content.len()).as_bytes());
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Nested [(array-printable) << /Inner (inner-printable) >>] \
              /Producer (",
        );
        pdf.extend_from_slice(producer);
        pdf.extend_from_slice(b") >>\nendobj\n");

        let size = offs.len() + 1;
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for off in &offs {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size {size} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    const STREAM_DICTIONARY_LABEL: &[u8] = b"stream-dict-printable";

    /// Assert the three strings in the AES fixtures' `/Info` object use raw
    /// hex string tokens after encryption. The source values are all printable
    /// and nested at scalar/array/dictionary positions, so literal `(...)`
    /// syntax anywhere in this object is a regression to generic emission.
    fn assert_encrypted_body_strings_are_hex(out: &[u8]) {
        let reopened = Pdf::open(Cursor::new(out.to_vec()))
            .expect("encrypted output must reopen with the empty user password");
        let info_ref = match reopened.trailer_dictionary().get("Info") {
            Some(Object::Reference(r)) => *r,
            other => panic!("trailer /Info must be a reference, got {other:?}"), // cov:ignore: fixture invariant
        };
        let info_bytes = find_object_bytes(out, info_ref.number);

        assert!(
            info_bytes
                .windows(b"/Nested [ <".len())
                .any(|part| part == b"/Nested [ <"),
            "nested array string must start with a hex token: {:?}",
            String::from_utf8_lossy(info_bytes) // cov:ignore: only evaluated on assertion failure
        );
        assert!(
            info_bytes
                .windows(b"/Inner <".len())
                .any(|part| part == b"/Inner <"),
            "nested dictionary string must start with a hex token: {:?}",
            String::from_utf8_lossy(info_bytes) // cov:ignore: only evaluated on assertion failure
        );
        assert!(
            info_bytes
                .windows(b"/Producer <".len())
                .any(|part| part == b"/Producer <"),
            "/Producer string must start with a hex token: {:?}",
            String::from_utf8_lossy(info_bytes) // cov:ignore: only evaluated on assertion failure
        );

        let literal_tokens = info_bytes.iter().filter(|&&byte| byte == b'(').count();
        let hex_tokens = info_bytes
            .iter()
            .enumerate()
            .filter(|&(index, byte)| {
                *byte == b'<'
                    && (index == 0 || info_bytes[index - 1] != b'<')
                    && info_bytes.get(index + 1) != Some(&b'<')
            })
            .count();
        assert_eq!(
            literal_tokens,
            0,
            "AES-encrypted /Info must contain no literal string token: {:?}",
            String::from_utf8_lossy(info_bytes) // cov:ignore: only evaluated on assertion failure
        );
        assert_eq!(
            hex_tokens,
            3,
            "all three AES-encrypted /Info strings must use hex tokens: {:?}",
            String::from_utf8_lossy(info_bytes) // cov:ignore: only evaluated on assertion failure
        );
    }

    /// Assert the page-content stream dictionary's printable `/Label` string
    /// is emitted as raw AES ciphertext hex and resolves back to its plaintext
    /// after the reader authenticates with the empty user password.
    fn assert_encrypted_stream_dictionary_string_is_hex_and_decrypts(out: &[u8]) {
        let mut reopened = Pdf::open(Cursor::new(out.to_vec()))
            .expect("encrypted output must reopen with the empty user password");
        let root_ref = reopened.root_ref().expect("root_ref");
        let pages_ref = match reopened.resolve_object(root_ref).expect("resolve /Root") {
            Object::Dictionary(d) => match d.get("Pages") {
                Some(Object::Reference(r)) => *r,
                other => panic!("/Pages must be a reference, got {other:?}"), // cov:ignore: fixture invariant
            },
            other => panic!("/Root must be a dictionary, got {other:?}"), // cov:ignore: fixture invariant
        };
        let page_ref = match reopened.resolve_object(pages_ref).expect("resolve /Pages") {
            Object::Dictionary(d) => match d.get("Kids") {
                Some(Object::Array(kids)) => match kids.first() {
                    Some(Object::Reference(r)) => *r,
                    other => panic!("Kids[0] must be a reference, got {other:?}"), // cov:ignore: fixture invariant
                },
                other => panic!("/Kids must be an array, got {other:?}"), // cov:ignore: fixture invariant
            },
            other => panic!("/Pages must be a dictionary, got {other:?}"), // cov:ignore: fixture invariant
        };
        let contents_ref = match reopened.resolve_object(page_ref).expect("resolve page") {
            Object::Dictionary(d) => match d.get("Contents") {
                Some(Object::Reference(r)) => *r,
                other => panic!("/Contents must be a reference, got {other:?}"), // cov:ignore: fixture invariant
            },
            other => panic!("page must be a dictionary, got {other:?}"), // cov:ignore: fixture invariant
        };

        let stream_bytes = find_object_bytes(out, contents_ref.number);
        assert!(
            stream_bytes
                .windows(b"/Label <".len())
                .any(|part| part == b"/Label <"),
            "stream dictionary /Label string must start with a hex token: {:?}",
            String::from_utf8_lossy(stream_bytes) // cov:ignore: only evaluated on assertion failure
        );
        assert!(
            !stream_bytes
                .windows(b"/Label (".len())
                .any(|part| part == b"/Label ("),
            "stream dictionary /Label must not use a literal token: {:?}",
            String::from_utf8_lossy(stream_bytes) // cov:ignore: only evaluated on assertion failure
        );

        let decrypted_label = match reopened
            .resolve_object(contents_ref)
            .expect("resolve encrypted /Contents")
        {
            Object::Stream(stream) => match stream.dict.get("Label") {
                Some(Object::String(label)) => label.clone(),
                other => panic!("stream /Label must be a string, got {other:?}"), // cov:ignore: fixture invariant
            },
            other => panic!("/Contents must be a stream, got {other:?}"), // cov:ignore: fixture invariant
        };
        assert_eq!(
            decrypted_label, STREAM_DICTIONARY_LABEL,
            "stream dictionary /Label must decrypt back to its original plaintext"
        );
    }

    /// Resolve `/Info /Producer` and the (single) page's `/Contents` stream
    /// payload — the two body values [`tiny_pdf_with_content_and_producer`]
    /// plants, read back through the ordinary `Pdf` reader API (which
    /// transparently decrypts when opened with the right password).
    fn resolve_producer_and_content<R: Read + Seek>(rt: &mut Pdf<R>) -> (Vec<u8>, Vec<u8>) {
        let info_ref = match rt.trailer_dictionary().get("Info") {
            Some(Object::Reference(r)) => *r,
            other => panic!("trailer /Info must be a reference, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
        };
        let producer = match rt.resolve_object(info_ref).expect("resolve /Info") {
            Object::Dictionary(d) => match d.get("Producer") {
                Some(Object::String(s)) => s.clone(),
                other => panic!("/Producer must be a string, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
            },
            other => panic!("/Info must be a dictionary, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
        };

        let root_ref = rt.root_ref().expect("root_ref");
        let pages_ref = match rt.resolve_object(root_ref).expect("resolve /Root") {
            Object::Dictionary(d) => match d.get("Pages") {
                Some(Object::Reference(r)) => *r,
                other => panic!("/Pages must be a reference, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
            },
            other => panic!("/Root must be a dictionary, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
        };
        let page_ref = match rt.resolve_object(pages_ref).expect("resolve /Pages") {
            Object::Dictionary(d) => match d.get("Kids") {
                Some(Object::Array(kids)) => match kids.first() {
                    Some(Object::Reference(r)) => *r,
                    other => panic!("Kids[0] must be a reference, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
                },
                other => panic!("/Kids must be an array, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
            },
            other => panic!("/Pages must be a dictionary, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
        };
        let contents_ref = match rt.resolve_object(page_ref).expect("resolve page") {
            Object::Dictionary(d) => match d.get("Contents") {
                Some(Object::Reference(r)) => *r,
                other => panic!("/Contents must be a reference, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
            },
            other => panic!("page must be a dictionary, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
        };
        let content = match rt.resolve_object(contents_ref).expect("resolve /Contents") {
            Object::Stream(s) => s.data,
            other => panic!("/Contents must be a stream, got {other:?}"), // cov:ignore: defensive fallback arm — never hit for either fixture's well-formed structure
        };
        (producer, content)
    }

    /// Task 8: `append_object`/`append_body_object` must actually encrypt
    /// every body object's strings and stream payloads, not merely leave a
    /// plaintext document underneath a declared `/Encrypt` dictionary (Task
    /// 6) and trailer reference (Task 7). This fixture carries a page
    /// content stream with a recognizable plaintext marker and an `/Info
    /// /Producer` string: both must be absent from the raw output bytes once
    /// encrypted, and — the actual security property, not just
    /// "ciphertext-shaped bytes appeared" — both must round-trip back to
    /// their exact original plaintext when the output is reopened with the
    /// correct password.
    #[test]
    fn linearize_with_encrypt_body_strings_and_streams_are_ciphertext() {
        let marker: &[u8] = b"BT /F1 12 Tf (flpdf linearize-encrypt content marker) Tj ET\n";
        let producer: &[u8] = b"flpdf linearize-encrypt producer marker";
        let src = tiny_pdf_with_content_and_producer(marker, producer);

        // Sanity/premise: with stream_data = Uncompress (no re-encoding) and
        // NO encryption, the content stream's raw bytes carry the marker
        // verbatim. This proves the fixture would actually leak plaintext if
        // encryption were a no-op, so the negative assertion below is
        // meaningful (not merely hidden by compression).
        let unencrypted = linearize_with(&src, |o| {
            o.stream_data = Some(crate::writer::StreamDataMode::Uncompress);
        });
        assert!(
            unencrypted.windows(marker.len()).any(|w| w == marker),
            "test premise: unencrypted output must carry the plaintext marker verbatim"
        );

        // Empty user password (qpdf's `--encrypt "" "" 128` convention, also
        // used by this file's other `/Encrypt`-emission tests): lets
        // `check_linearization_bytes` below — which opens with no password —
        // and the explicit reopen further down both decrypt transparently.
        let out = linearize_with(&src, |o| {
            o.stream_data = Some(crate::writer::StreamDataMode::Uncompress);
            o.static_aes_iv = true;
            o.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
                Vec::new(),
                b"owner".to_vec(),
            ));
        });

        assert!(
            !out.windows(marker.len()).any(|w| w == marker),
            "content stream plaintext leaked into encrypted linearized output"
        );
        assert!(
            !out.windows(producer.len()).any(|w| w == producer),
            "/Info /Producer plaintext leaked into encrypted linearized output"
        );

        // The /Encrypt dictionary itself must stay plaintext (Task 6): the
        // reader has to parse it before it can derive the file key.
        let standard_needle: &[u8] = b"/Filter /Standard";
        assert!(
            out.windows(standard_needle.len())
                .any(|w| w == standard_needle),
            "/Encrypt dictionary must remain plaintext"
        );

        crate::linearization::check_linearization_bytes(&out)
            .expect("encrypted linearized output must still pass the linearization checker");

        // The real security property: reopening (with the empty user
        // password) decrypts BOTH the string and the stream back to their
        // exact original plaintext (proves the xref table itself also
        // stayed plaintext and correctly locates every object — a garbled
        // or encrypted xref table would fail this reopen outright).
        let mut reopened =
            Pdf::open_with_options(Cursor::new(out.clone()), crate::PdfOpenOptions::default())
                .expect("re-open of encrypted linearized output with the empty user password");
        let (decrypted_producer, decrypted_content) = resolve_producer_and_content(&mut reopened);
        assert_eq!(
            decrypted_producer, producer,
            "/Info /Producer must decrypt back to its original plaintext"
        );
        assert_eq!(
            decrypted_content, marker,
            "content stream must decrypt back to its original plaintext"
        );
    }

    #[test]
    fn linearized_aes_strings_use_hex() {
        let src = tiny_pdf_with_content_and_producer(
            b"BT (linearized AES string syntax) Tj ET\n",
            b"producer-printable",
        );
        let out = linearize_with(&src, |options| {
            options.static_aes_iv = true;
            options.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
                Vec::new(),
                b"owner".to_vec(),
            ));
        });

        assert_encrypted_body_strings_are_hex(&out);
        assert_encrypted_stream_dictionary_string_is_hex_and_decrypts(&out);
        crate::linearization::check_linearization_bytes(&out)
            .expect("AES-encrypted output must pass linearization checks");
    }

    /// Task 11: every prior test in this module (Tasks 6-10) only exercised
    /// `EncryptMethod::V4Aes128`. V=5 R=6 AES-256 takes a genuinely
    /// different code path through `crate::writer::build_encryption_context`
    /// — `WriteCipher::FileKeyAes256` uses the 32-byte file key directly for
    /// every object, with no per-object Algorithm-1 derivation and no
    /// `/ID[0]` dependency (unlike V=4's `WriteCipher::PerObject`; see
    /// `WriteCipher`'s own doc) — so this proves the linearized writer's
    /// generic dispatch on `EncryptParams::method` actually reaches that
    /// path, not just that V=4 works.
    ///
    /// Assertion style mirrors two existing precedents: the on-disk
    /// `/Encrypt` dict shape check follows
    /// `linearize_with_encrypt_emits_encrypt_dict_at_reserved_object_number`
    /// (locate the reserved object, inspect its raw bytes); the correctness
    /// gate follows `crate::writer::tests::v5_r6_encrypt_round_trips_string_and_stream_via_reader`
    /// — V=5's random salts + FEK give no byte-identical determinism to
    /// assert, so password round-trip decryption is the real proof.
    #[test]
    fn linearize_with_v5_r6_aes256_encrypts_and_round_trips() {
        let content_marker: &[u8] = b"BT /F1 12 Tf (flpdf v5r6 linearize content marker) Tj ET\n";
        let producer_marker: &[u8] = b"flpdf v5r6 linearize producer marker";
        let src = tiny_pdf_with_content_and_producer(content_marker, producer_marker);

        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let expected_encrypt_num = renumber.hint_stream_slot();

        let out = linearize_with(&src, |o| {
            o.stream_data = Some(crate::writer::StreamDataMode::Uncompress);
            o.static_aes_iv = true;
            o.encrypt = Some(crate::encryption::EncryptParams::v5_r6(
                Vec::new(),
                b"owner".to_vec(),
            ));
        });

        // Premise (verified for this same fixture shape by the V=4 sibling
        // `linearize_with_encrypt_body_strings_and_streams_are_ciphertext`,
        // which asserts the marker DOES appear verbatim in unencrypted
        // output under the same `stream_data = Uncompress`): these negative
        // checks are supplementary, not load-bearing — the round-trip
        // decrypt below is the actual correctness proof.
        assert!(
            !out.windows(content_marker.len())
                .any(|w| w == content_marker),
            "content stream plaintext leaked into V=5 R=6 encrypted linearized output"
        );
        assert!(
            !out.windows(producer_marker.len())
                .any(|w| w == producer_marker),
            "/Info /Producer plaintext leaked into V=5 R=6 encrypted linearized output"
        );

        let encrypt_object_bytes = find_object_bytes(&out, expected_encrypt_num);
        for needle in [
            b"/V 5".as_slice(),
            b"/R 6".as_slice(),
            b"/CFM /AESV3".as_slice(),
        ] {
            assert!(
                encrypt_object_bytes
                    .windows(needle.len())
                    .any(|w| w == needle),
                "V=5 R=6 /Encrypt dict must contain {needle:?}"
            );
        }

        // V=5 R=6 floors the Adobe developer extension level to 8 at
        // `/Extensions /ADBE /ExtensionLevel` on the CATALOG (qpdf
        // QPDFWriter.cc L806-808's `setMinimumPDFVersion("1.7", 8)`,
        // L1355-1450's `addDeveloperExtension` — verified byte-identical
        // against real qpdf 11.9.0's `--linearize --encrypt "" owner 256
        // --static-id --static-aes-iv --` output for this exact fixture
        // shape). This was flpdf-txag's Task 11 review-found gap:
        // `write_linearized` wired the header-version half
        // (`effective_pdf_version`) but never the Catalog-injection half
        // that `crate::writer::emit_canonical_pdf_inner` already had
        // (writer.rs:3154-3238).
        let catalog_new_ref = renumber
            .new_for_original(plan.root_ref.expect("plan has root_ref"))
            .expect("catalog must be in the renumber map");
        let catalog_object_bytes = find_object_bytes(&out, catalog_new_ref.number);
        for needle in [
            b"/Extensions".as_slice(),
            b"/ADBE".as_slice(),
            b"/BaseVersion /1.7".as_slice(),
            b"/ExtensionLevel 8".as_slice(),
        ] {
            assert!(
                catalog_object_bytes
                    .windows(needle.len())
                    .any(|w| w == needle),
                "V=5 R=6 Catalog must contain {needle:?}"
            );
        }

        crate::linearization::check_linearization_bytes(&out)
            .expect("V=5 R=6 encrypted linearized output must pass the linearization checker");

        for pw in [b"".as_slice(), b"owner".as_slice()] {
            let mut reopened = Pdf::open_with_options(
                Cursor::new(out.clone()),
                crate::PdfOpenOptions {
                    password: pw.to_vec(),
                    ..crate::PdfOpenOptions::default()
                },
            )
            .unwrap_or_else(|e| {
                // cov:ignore-start: only reached if the reopen fails — the
                // property under test is that it always succeeds for a
                // correctly encrypted V=5 R=6 output.
                panic!("re-open of V=5 R=6 linearized output with password {pw:?} failed: {e}")
                // cov:ignore-end
            });
            let (producer, content) = resolve_producer_and_content(&mut reopened);
            assert_eq!(
                producer, producer_marker,
                "V=5 R=6 /Info /Producer must decrypt back to its original plaintext"
            );
            assert_eq!(
                content, content_marker,
                "V=5 R=6 content stream must decrypt back to its original plaintext"
            );
        }
    }

    /// As [`linearize_with_v5_r6_aes256_encrypts_and_round_trips`], but for
    /// V=5 R=5 (deprecated pre-ISO 32000-2 AES-256, `EncryptParams::v5_r5`,
    /// selected by `--force-R5`). R=5 shares V=5's `WriteCipher::
    /// FileKeyAes256` code path with R=6 (only the password-hash algorithm
    /// and the `/R` value differ — see `build_v5_r5_encrypt_dict`'s doc), so
    /// this is not a redundant re-run of the R=6 case: it is R=5's own
    /// deprecated encryption revision that needs its own coverage, mirroring
    /// `crate::writer::tests::v5_r5_encrypt_round_trips_string_and_stream_via_reader`.
    /// `check_linearization_bytes` opens with the reader's default options,
    /// which accept weak encrypted inputs just like qpdf, and calls
    /// `check_linearization` directly instead of reopening through a CLI job.
    #[test]
    fn linearize_with_v5_r5_aes256_encrypts_and_round_trips() {
        let content_marker: &[u8] = b"BT /F1 12 Tf (flpdf v5r5 linearize content marker) Tj ET\n";
        let producer_marker: &[u8] = b"flpdf v5r5 linearize producer marker";
        let src = tiny_pdf_with_content_and_producer(content_marker, producer_marker);

        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let expected_encrypt_num = renumber.hint_stream_slot();

        let out = linearize_with(&src, |o| {
            o.stream_data = Some(crate::writer::StreamDataMode::Uncompress);
            o.static_aes_iv = true;
            o.encrypt = Some(crate::encryption::EncryptParams::v5_r5(
                Vec::new(),
                b"owner".to_vec(),
            ));
        });

        // See the R=6 sibling test's identical comment: the premise (marker
        // appears verbatim under stream_data = Uncompress with no
        // encryption) is verified by
        // `linearize_with_encrypt_body_strings_and_streams_are_ciphertext`
        // on the same fixture shape; these negative checks are
        // supplementary, the round-trip decrypt below is load-bearing.
        assert!(
            !out.windows(content_marker.len())
                .any(|w| w == content_marker),
            "content stream plaintext leaked into V=5 R=5 encrypted linearized output"
        );
        assert!(
            !out.windows(producer_marker.len())
                .any(|w| w == producer_marker),
            "/Info /Producer plaintext leaked into V=5 R=5 encrypted linearized output"
        );

        let encrypt_object_bytes = find_object_bytes(&out, expected_encrypt_num);
        for needle in [
            b"/V 5".as_slice(),
            b"/R 5".as_slice(),
            b"/CFM /AESV3".as_slice(),
        ] {
            assert!(
                encrypt_object_bytes
                    .windows(needle.len())
                    .any(|w| w == needle),
                "V=5 R=5 /Encrypt dict must contain {needle:?}"
            );
        }

        // V=5 R=5 floors the Adobe developer extension level to 3 (qpdf
        // QPDFWriter.cc L806-808's `setMinimumPDFVersion("1.7", 3)`) — see
        // the R=6 sibling test's identical comment for the full citation and
        // the real-qpdf byte-identical verification this mirrors.
        let catalog_new_ref = renumber
            .new_for_original(plan.root_ref.expect("plan has root_ref"))
            .expect("catalog must be in the renumber map");
        let catalog_object_bytes = find_object_bytes(&out, catalog_new_ref.number);
        for needle in [
            b"/Extensions".as_slice(),
            b"/ADBE".as_slice(),
            b"/BaseVersion /1.7".as_slice(),
            b"/ExtensionLevel 3".as_slice(),
        ] {
            assert!(
                catalog_object_bytes
                    .windows(needle.len())
                    .any(|w| w == needle),
                "V=5 R=5 Catalog must contain {needle:?}"
            );
        }

        let mut checker_pdf = Pdf::open_with_options(
            Cursor::new(out.clone()),
            crate::PdfOpenOptions {
                ..crate::PdfOpenOptions::default()
            },
        )
        .expect("re-open with the empty user password for the checker");
        crate::linearization::check_linearization(&mut checker_pdf, &out)
            .expect("V=5 R=5 encrypted linearized output must pass the linearization checker");

        for pw in [b"".as_slice(), b"owner".as_slice()] {
            let mut reopened = Pdf::open_with_options(
                Cursor::new(out.clone()),
                crate::PdfOpenOptions {
                    password: pw.to_vec(),
                    ..crate::PdfOpenOptions::default()
                },
            )
            .unwrap_or_else(|e| {
                // cov:ignore-start: only reached if the reopen fails — the
                // property under test is that it always succeeds for a
                // correctly encrypted V=5 R=5 output.
                panic!("re-open of V=5 R=5 linearized output with password {pw:?} failed: {e}")
                // cov:ignore-end
            });
            let (producer, content) = resolve_producer_and_content(&mut reopened);
            assert_eq!(
                producer, producer_marker,
                "V=5 R=5 /Info /Producer must decrypt back to its original plaintext"
            );
            assert_eq!(
                content, content_marker,
                "V=5 R=5 content stream must decrypt back to its original plaintext"
            );
        }
    }

    /// Minimal one-page PDF whose Catalog carries `/Extensions` as an
    /// INDIRECT reference (object 4) to `<< /ADBE << /BaseVersion /1.6
    /// /ExtensionLevel 2 >> >>` — the scope-out case
    /// [`resolve_catalog_adbe_status`] rejects (top-level `/Extensions`
    /// indirection).
    fn tiny_pdf_with_indirect_extensions_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"4 0 obj\n<< /ADBE << /BaseVersion /1.6 /ExtensionLevel 2 >> >>\nendobj\n",
        );

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

    /// Task 11 review fix: a source Catalog `/Extensions` stored as an
    /// indirect reference must be rejected loudly, not silently inlined —
    /// see [`CatalogAdbeStatus::orphans_indirect_object`]'s doc for why
    /// (this function's `plan`/`renumber` are already frozen from a
    /// separate `Pdf` handle by the time this runs, unlike
    /// `crate::writer::emit_canonical_pdf_inner`, which mutates the
    /// Catalog before its OWN renumbering). V=5 R=6 encryption's ext-8
    /// floor (`eff_ext > 0`) is what makes this fixture actually need to
    /// touch `/Extensions` at all — a non-encrypting linearize of the same
    /// fixture takes neither the inject nor the reject branch, since its
    /// own ext(2)-vs-source(2) pairwise result never changes.
    #[test]
    fn linearize_encrypt_v5_rejects_indirect_source_extensions() {
        let src = tiny_pdf_with_indirect_extensions_bytes();
        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            encrypt: Some(crate::encryption::EncryptParams::v5_r6(
                Vec::new(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut pdf2 = Pdf::open(Cursor::new(src)).expect("source parses");
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("indirect reference")),
            "got {err:?}"
        );
    }

    /// Minimal one-page PDF whose Catalog carries `/Extensions` as a
    /// DIRECT dictionary, but whose `/ADBE` entry WITHIN that dictionary is
    /// itself an INDIRECT reference (object 4) to `<< /BaseVersion /1.6
    /// /ExtensionLevel 2 >>` — the nested-indirection scope-out case
    /// [`resolve_catalog_adbe_status`] rejects. Unlike
    /// [`tiny_pdf_with_indirect_extensions_bytes`], `/Extensions` itself
    /// never moves; only its `/ADBE` entry does.
    fn tiny_pdf_with_indirect_adbe_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions << /ADBE 4 0 R >> >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"4 0 obj\n<< /BaseVersion /1.6 /ExtensionLevel 2 >>\nendobj\n");

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

    /// Code-quality review follow-up to the previous fix: the top-level
    /// indirect-`/Extensions` guard alone missed a case one level deeper.
    /// `inject_adbe_extension`/`strip_adbe_extension` both unconditionally
    /// overwrite (`extensions.insert("ADBE", ..)`) or remove
    /// (`extensions.remove("ADBE")`) the `/ADBE` entry without ever
    /// resolving the OLD value first — so a DIRECT `/Extensions` dict whose
    /// `/ADBE` entry is itself an indirect reference orphans that object
    /// exactly the same way a top-level indirect `/Extensions` does, even
    /// though `/Extensions` itself never moves. Before the fix this
    /// combination silently succeeded, injecting the correct new
    /// `/Extensions` while ALSO leaving a dangling orphaned body object
    /// behind — a genuine byte/structural divergence from real qpdf, which
    /// never enqueues that object in the first place (mutate-then-renumber
    /// on one handle). Mirrors
    /// [`linearize_encrypt_v5_rejects_indirect_source_extensions`], with
    /// the nested fixture in place of the top-level one.
    #[test]
    fn linearize_encrypt_v5_rejects_nested_indirect_adbe() {
        let src = tiny_pdf_with_indirect_adbe_bytes();
        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            encrypt: Some(crate::encryption::EncryptParams::v5_r6(
                Vec::new(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut pdf2 = Pdf::open(Cursor::new(src)).expect("source parses");
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("indirect reference")),
            "got {err:?}"
        );
    }

    /// Minimal one-page PDF whose Catalog carries `/Extensions` as an
    /// INDIRECT reference (object 4) to a value that is NOT a dictionary at
    /// all — a bare integer. Regression fixture: a top-level indirect
    /// `/Extensions` must be rejected purely because it is indirect,
    /// regardless of what it resolves to. `resolve_catalog_adbe_status`
    /// decides this via `collect_direct_refs` on the UNRESOLVED
    /// `/Extensions` value (which pushes the reference itself before ever
    /// resolving it), so a non-Dictionary target is caught exactly like a
    /// well-formed one — unlike an earlier, now-corrected version of the
    /// check that resolved first and only flagged the reference when the
    /// resolved target happened to be a Dictionary.
    fn tiny_pdf_with_indirect_extensions_to_non_dict_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions 4 0 R >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"4 0 obj\n42\nendobj\n");

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

    /// Code-quality review follow-up: `resolve_catalog_adbe_status` must
    /// keep rejecting an indirect `/Extensions` even when the resolved
    /// target isn't a Dictionary — a prior revision of the check resolved
    /// the indirect `/Extensions` first and only flagged the orphan risk
    /// when the resolved value was itself `Some(Dictionary)`, so a
    /// non-Dictionary target silently fell through as "safe" (worse than
    /// the ORIGINAL top-level-only check this crate started with, which was
    /// type-agnostic and rejected on `Object::Reference` alone without
    /// caring what it resolved to). See
    /// [`linearize_encrypt_v5_rejects_indirect_source_extensions`] for the
    /// well-formed sibling of this fixture.
    #[test]
    fn linearize_encrypt_v5_rejects_indirect_source_extensions_to_non_dict() {
        let src = tiny_pdf_with_indirect_extensions_to_non_dict_bytes();
        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            encrypt: Some(crate::encryption::EncryptParams::v5_r6(
                Vec::new(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut pdf2 = Pdf::open(Cursor::new(src)).expect("source parses");
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("indirect reference")),
            "got {err:?}"
        );
    }

    /// Minimal one-page PDF whose Catalog carries `/Extensions` as a DIRECT
    /// dictionary with a DIRECT `/ADBE` dictionary, but whose `/ADBE`
    /// dictionary carries an `/ExtensionLevel` value that is itself an
    /// INDIRECT reference (object 4) to `2` — one level deeper than
    /// [`tiny_pdf_with_indirect_adbe_bytes`], where `/ADBE` itself is the
    /// indirect entry. Here neither `/Extensions` nor `/ADBE` ever moves;
    /// only a value nested inside `/ADBE` is indirect.
    fn tiny_pdf_with_indirect_extension_level_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions << /ADBE \
              << /BaseVersion /1.6 /ExtensionLevel 4 0 R >> >> >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"4 0 obj\n2\nendobj\n");

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

    /// Code-quality review follow-up: the top-level-`/Extensions` and
    /// direct-`/Extensions`-with-indirect-`/ADBE` guards alone still missed
    /// a case one level deeper still — a fully direct `/Extensions` and
    /// `/ADBE`, but an indirect VALUE nested inside `/ADBE` (here
    /// `/ExtensionLevel`). `inject_adbe_extension` replaces the entire
    /// `/ADBE` dictionary wholesale (`adbe.insert("ExtensionLevel", ..)` on
    /// a FRESH `Dictionary`, discarding the old one), so this reference is
    /// orphaned exactly like the shallower cases even though `/Extensions`
    /// and `/ADBE` both stay direct. `resolve_catalog_adbe_status` now
    /// catches this — and any further nesting depth — via a single
    /// shape-independent [`collect_direct_refs`] walk over the whole
    /// `/Extensions` subtree, rather than a growing list of enumerated
    /// shapes. Mirrors
    /// [`linearize_encrypt_v5_rejects_nested_indirect_adbe`], one level
    /// deeper.
    #[test]
    fn linearize_encrypt_v5_rejects_indirect_extension_level() {
        let src = tiny_pdf_with_indirect_extension_level_bytes();
        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            encrypt: Some(crate::encryption::EncryptParams::v5_r6(
                Vec::new(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut pdf2 = Pdf::open(Cursor::new(src)).expect("source parses");
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("indirect reference")),
            "got {err:?}"
        );
    }

    /// Minimal one-page PDF whose Catalog carries `/Extensions` as a DIRECT
    /// Array (not a Dictionary at all — non-conformant per ISO 32000, which
    /// defines `/Extensions` as a dictionary) with one element being an
    /// INDIRECT reference (object 4). Regression fixture for
    /// `resolve_catalog_adbe_status`'s catch-all match arm (`/Extensions`
    /// present but neither `Object::Dictionary` nor `Object::Reference`):
    /// `has_adbe` is unconditionally `false` there (there is no dict to
    /// look `/ADBE` up in), but `collect_direct_refs` still walks the array
    /// element and must still flag the nested reference.
    fn tiny_pdf_with_array_extensions_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions [4 0 R] >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"4 0 obj\n<< /BaseVersion /1.6 /ExtensionLevel 2 >>\nendobj\n");

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

    /// Code-quality review follow-up: exercises `resolve_catalog_adbe_status`'s
    /// catch-all match arm (`/Extensions` present but neither `Dictionary`
    /// nor `Reference`) with a committed fixture, which is what lets the
    /// coverage-exclusion marker this arm previously carried be dropped. An
    /// Array-shaped `/Extensions` is non-conformant per ISO 32000
    /// (`/Extensions` is defined as a dictionary), so real qpdf never has to
    /// handle this shape from a conforming file; flpdf still rejects the
    /// indirect reference inside it defensively rather than mis-handling
    /// untrusted input, which is a flpdf scope limitation, not a parity
    /// requirement.
    #[test]
    fn linearize_encrypt_v5_rejects_array_extensions_with_indirect_element() {
        let src = tiny_pdf_with_array_extensions_bytes();
        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            encrypt: Some(crate::encryption::EncryptParams::v5_r6(
                Vec::new(),
                b"owner".to_vec(),
            )),
            ..WriterOptions::default()
        };
        let mut pdf2 = Pdf::open(Cursor::new(src)).expect("source parses");
        let err = write_linearized(&plan, &renumber, &mut pdf2, &opts).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("indirect reference")),
            "got {err:?}"
        );
    }

    /// Minimal one-page PDF whose Catalog carries a DIRECT `/Extensions`
    /// dict with a malformed `/ADBE` entry (no `/ExtensionLevel` key at
    /// all) — qpdf removes `/ADBE` based on key existence, not
    /// `/ExtensionLevel` validity (QPDFWriter.cc L1387), so this must be
    /// stripped whenever the effective extension level is 0, even without
    /// any version race.
    fn tiny_pdf_with_malformed_direct_extensions_bytes() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Extensions << /ADBE << >> >> >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

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

    /// Task 11 review fix, the STRIP arm: a non-encrypting linearize (no
    /// `--min-extension-level`, so the effective extension level is 0) of a
    /// source whose Catalog already carries a stale/malformed direct
    /// `/Extensions /ADBE` must have that key removed from the output —
    /// mirrors `crate::writer::strip_adbe_extension`'s doc and its
    /// full-rewrite byte-parity precedent, now wired through
    /// `write_linearized` too.
    #[test]
    fn linearize_strips_malformed_direct_source_extensions_when_no_ext_requested() {
        let src = tiny_pdf_with_malformed_direct_extensions_bytes();
        let out = linearize_with(&src, |_o| {});
        let needle: &[u8] = b"/Extensions";
        assert!(
            !out.windows(needle.len()).any(|w| w == needle),
            "stale /Extensions /ADBE (no /ExtensionLevel) must be stripped when no \
             effective extension level is requested"
        );
        crate::linearization::check_linearization_bytes(&out)
            .expect("output with stripped /Extensions must still pass the linearization checker");
    }

    /// Build a linearizable single-page PDF whose Catalog carries a
    /// `/Metadata` XMP stream (raw `metadata_xml` bytes, no `/Filter`) in
    /// addition to a page content stream (raw `content` bytes) and an
    /// `/Info /Producer` string — three distinct body values with three
    /// distinct expected encryption outcomes under `--cleartext-metadata`
    /// (`encrypt_metadata: false`): the metadata stream must be exempted,
    /// the content stream and the producer string must not be.
    fn tiny_pdf_with_metadata_content_and_producer(
        metadata_xml: &[u8],
        content: &[u8],
        producer: &[u8],
    ) -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let mut offs = Vec::new();

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Contents 4 0 R >>\nendobj\n",
        );

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes(),
        );
        pdf.extend_from_slice(content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(b"5 0 obj\n<< /Producer (");
        pdf.extend_from_slice(producer);
        pdf.extend_from_slice(b") >>\nendobj\n");

        offs.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!(
                "6 0 obj\n<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n",
                metadata_xml.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(metadata_xml);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        let size = offs.len() + 1;
        let xref_start = pdf.len() as u64;
        let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for off in &offs {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        pdf.extend_from_slice(xref.as_bytes());
        let trailer = format!(
            "trailer\n<< /Size {size} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{xref_start}\n%%EOF\n"
        );
        pdf.extend_from_slice(trailer.as_bytes());
        pdf
    }

    /// Task 8 follow-up: `--cleartext-metadata` (`encrypt_metadata: false`)
    /// must exempt ONLY the `/Catalog /Metadata` XMP stream from encryption
    /// (leaving it in the clear with `/Crypt /Identity` prepended, mirroring
    /// `crate::writer::emit_canonical_pdf`'s `Object::Stream` branch,
    /// `writer.rs` around line 3826) — every OTHER body string/stream must
    /// still be encrypted normally. A same-fixture A/B check: the metadata
    /// marker stays readable, the content-stream marker and the `/Info
    /// /Producer` string do not.
    #[test]
    fn linearize_with_encrypt_cleartext_metadata_exempts_only_metadata_stream() {
        let metadata_marker: &[u8] = b"<?xpacket flpdf-cleartext-metadata-xml-marker?>";
        let content_marker: &[u8] =
            b"BT /F1 12 Tf (flpdf cleartext-metadata content marker) Tj ET\n";
        let producer_marker: &[u8] = b"flpdf cleartext-metadata producer marker";
        let src = tiny_pdf_with_metadata_content_and_producer(
            metadata_marker,
            content_marker,
            producer_marker,
        );

        // Empty user password (qpdf's `--encrypt "" "" 128` convention) so
        // both `check_linearization_bytes` (no-password open) and the
        // explicit reopen below can decrypt transparently.
        let out = linearize_with(&src, |o| {
            o.stream_data = Some(crate::writer::StreamDataMode::Uncompress);
            o.static_aes_iv = true;
            o.encrypt = Some(crate::encryption::EncryptParams {
                encrypt_metadata: false,
                ..crate::encryption::EncryptParams::v4_aes128(Vec::new(), b"owner".to_vec())
            });
        });

        // (a) The exempted /Metadata stream's marker is readable in the raw
        // output — never ran through the cipher.
        assert!(
            out.windows(metadata_marker.len())
                .any(|w| w == metadata_marker),
            "cleartext-metadata: the /Metadata XMP stream must stay plaintext, \
             got {:?}",
            String::from_utf8_lossy(&out) // cov:ignore: only evaluated when the assertion above fails.
        );

        // (b) Its dict carries /Crypt /Identity (the exact form
        // prepend_crypt_filter_to_stream_dict produces for a source with no
        // prior /Filter: a singleton /Filter /Crypt plus a
        // /DecodeParms << /Type /CryptFilterDecodeParms /Name /Identity >>),
        // so a reader knows not to attempt decryption.
        let crypt_needle: &[u8] = b"/Filter /Crypt";
        assert!(
            out.windows(crypt_needle.len()).any(|w| w == crypt_needle),
            "cleartext-metadata: /Metadata dict must carry /Filter /Crypt"
        );
        let identity_needle: &[u8] = b"/Name /Identity";
        assert!(
            out.windows(identity_needle.len())
                .any(|w| w == identity_needle),
            "cleartext-metadata: /Metadata dict must carry /Name /Identity in /DecodeParms"
        );

        // (c) Every OTHER body value is still properly encrypted — the
        // exemption is scoped to metadata_ref alone, not blanket-applied.
        assert!(
            !out.windows(content_marker.len())
                .any(|w| w == content_marker),
            "cleartext-metadata must not exempt the page content stream"
        );
        assert!(
            !out.windows(producer_marker.len())
                .any(|w| w == producer_marker),
            "cleartext-metadata must not exempt the /Info /Producer string"
        );

        crate::linearization::check_linearization_bytes(&out).expect(
            "cleartext-metadata linearized output must still pass the linearization checker",
        );

        // Reader round-trip: the content stream and /Info /Producer decrypt
        // back to their originals (proving they really were encrypted, not
        // merely absent-by-coincidence), and the /Metadata stream resolves
        // to its ORIGINAL bytes too — via the /Crypt /Identity passthrough,
        // not via decryption (there is nothing to decrypt).
        let mut reopened =
            Pdf::open_with_options(Cursor::new(out.clone()), crate::PdfOpenOptions::default())
                .expect(
                    "re-open of cleartext-metadata linearized output with the empty user password",
                );
        let (decrypted_producer, decrypted_content) = resolve_producer_and_content(&mut reopened);
        assert_eq!(
            decrypted_producer, producer_marker,
            "/Info /Producer must decrypt back to its original plaintext"
        );
        assert_eq!(
            decrypted_content, content_marker,
            "content stream must decrypt back to its original plaintext"
        );

        let root_ref = reopened.root_ref().expect("root_ref");
        let metadata_ref = match reopened.resolve_object(root_ref).expect("resolve /Root") {
            Object::Dictionary(d) => match d.get_ref("Metadata") {
                Some(r) => r,
                None => panic!("/Root must carry /Metadata"), // cov:ignore: only evaluated when the assertion above fails.
            },
            other => panic!("/Root must be a dictionary, got {other:?}"), // cov:ignore: only evaluated when the assertion above fails.
        };
        let metadata_bytes = match reopened
            .resolve_object(metadata_ref)
            .expect("resolve /Metadata")
        {
            Object::Stream(s) => s.data,
            other => panic!("/Metadata must be a stream, got {other:?}"), // cov:ignore: only evaluated when the assertion above fails.
        };
        assert_eq!(
            metadata_bytes, metadata_marker,
            "/Metadata stream must resolve to its original plaintext via /Crypt /Identity"
        );
    }

    /// Locate object `number`'s raw byte range in `out` — from its `\n{number}
    /// 0 obj` header (inclusive) to just before the next `endobj` (exclusive).
    /// Every object header in this writer is preceded by a newline
    /// (`append_object`/`append_body_object` emit `\nendobj\n` for the prior
    /// object, or a trailer/xref section ends in `\n`), so search for
    /// `\n{number} 0 obj` rather than `{number} 0 obj` alone — an unprefixed
    /// search for e.g. "4 0 obj" would also match inside "14 0 obj" for
    /// larger fixtures.
    fn find_object_bytes(out: &[u8], number: u32) -> &[u8] {
        let header = format!("\n{number} 0 obj");
        let header_pos = out
            .windows(header.len())
            .position(|w| w == header.as_bytes())
            .unwrap_or_else(|| {
                // cov:ignore-start: only reached if a caller's own assertion
                // would already fail — every well-formed fixture in this
                // test module produces a findable header for its own refs.
                panic!("expected \"{header}\" (object {number}'s header) in output")
                // cov:ignore-end
            })
            + 1;
        let body = &out[header_pos..];
        let body_end = body
            .windows(b"endobj".len())
            .position(|w| w == b"endobj")
            .unwrap_or(body.len());
        &body[..body_end]
    }

    /// Locate object `number`'s start byte OFFSET in `out` (the position of
    /// its `{number} 0 obj` header). Sibling of [`find_object_bytes`], which
    /// returns the object's body slice instead of its offset.
    fn find_object_offset(out: &[u8], number: u32) -> usize {
        let header = format!("\n{number} 0 obj");
        out.windows(header.len())
            .position(|w| w == header.as_bytes())
            .unwrap_or_else(|| panic!("expected \"{header}\" (object {number}'s header) in output"))
            + 1
    }

    /// `--cleartext-metadata` must use qpdf's metadata policy even when the
    /// general stream option requests compression. `QPDFWriter::willFilterStream`
    /// takes the metadata branch before the normal compression branch
    /// (`QPDFWriter.cc:1274-1284`), decodes the stream fully, and emits the
    /// cleartext payload with the explicit `/Crypt /Identity` stage added by
    /// the linearized writer. The old compatibility route compressed this
    /// stream first and produced `[/Crypt /FlateDecode]`; qpdf does not.
    #[test]
    fn linearize_with_encrypt_cleartext_metadata_uses_qpdf_uncompressed_policy() {
        let metadata_marker: &[u8] = b"<?xpacket flpdf-refilter-metadata-marker?>";
        let src = tiny_pdf_with_metadata_content_and_producer(
            metadata_marker,
            b"BT /F1 12 Tf (flpdf refilter content marker) Tj ET\n",
            b"flpdf refilter producer marker",
        );

        // Keep the general compression request explicit. qpdf's metadata
        // branch must override it and emit the cleartext payload uncompressed.
        let out = linearize_with(&src, |o| {
            o.stream_data = Some(crate::writer::StreamDataMode::Compress);
            o.static_aes_iv = true;
            o.encrypt = Some(crate::encryption::EncryptParams {
                encrypt_metadata: false,
                ..crate::encryption::EncryptParams::v4_aes128(Vec::new(), b"owner".to_vec())
            });
        });

        crate::linearization::check_linearization_bytes(&out).expect(
            "cleartext-metadata + default compress policy must still pass the linearization checker",
        );

        let mut reopened =
            Pdf::open_with_options(Cursor::new(out.clone()), crate::PdfOpenOptions::default())
                .expect("re-open with the empty user password");
        let root_ref = reopened.root_ref().expect("root_ref");
        let metadata_ref = match reopened.resolve_object(root_ref).expect("resolve /Root") {
            Object::Dictionary(d) => match d.get_ref("Metadata") {
                Some(r) => r,
                None => panic!("/Root must carry /Metadata"), // cov:ignore: only evaluated when the assertion above fails.
            },
            other => panic!("/Root must be a dictionary, got {other:?}"), // cov:ignore: only evaluated when the assertion above fails.
        };

        let object_bytes = find_object_bytes(&out, metadata_ref.number);

        // The metadata stream must carry the explicit identity Crypt stage,
        // but qpdf does not retain a Flate stage after its uncompress branch.
        let filter_needle: &[u8] = b"/Filter /Crypt";
        assert!(
            object_bytes
                .windows(filter_needle.len())
                .any(|w| w == filter_needle),
            "cleartext /Metadata stream must carry /Crypt /Identity in \
             its filter dictionary, got {:?}",
            String::from_utf8_lossy(object_bytes) // cov:ignore: only evaluated when the assertion above fails.
        );

        // Negative check: the metadata policy must not retain a Flate stage.
        let bare_flate_needle: &[u8] = b"/Filter /FlateDecode";
        assert!(
            !object_bytes
                .windows(bare_flate_needle.len())
                .any(|w| w == bare_flate_needle),
            "cleartext /Metadata stream must not retain /FlateDecode, \
             got {:?}",
            String::from_utf8_lossy(object_bytes) // cov:ignore: only evaluated when the assertion above fails.
        );

        // Reader round-trip. `decrypt_resolved_object` (reader.rs) has its
        // own `/Type /Metadata` + `!encrypt_metadata` fast path, so it keeps
        // the raw on-disk `/Crypt` identity filter and plaintext bytes.
        let resolved = reopened
            .resolve_object(metadata_ref)
            .expect("resolve /Metadata (reader's metadata fast path leaves it untouched)");
        let Object::Stream(s) = resolved else {
            // cov:ignore-start: only reached if the assertion below would
            // already fail — /Metadata always resolves to a stream for this
            // fixture's well-formed structure.
            panic!("/Metadata must resolve to a stream, got {resolved:?}");
            // cov:ignore-end
        };
        assert_eq!(
            s.dict.get("Filter"),
            Some(&Object::Name(b"Crypt".to_vec())),
            "resolved /Metadata dict must still carry the raw on-disk \
             /Crypt identity filter (the reader's metadata fast path \
             skips /Crypt stripping entirely)"
        );
        assert_eq!(
            s.data, metadata_marker,
            "/Metadata stream must retain its original plaintext bytes"
        );
    }

    #[test]
    fn append_body_object_emits_encrypted_stream_dictionary() {
        use crate::writer::{EncryptionContext, WriteCipher};

        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 16],
            cipher: WriteCipher::PerObject(crate::ObjectKeyAlg::Aes),
            encryption_v: 4,
            encryption_r: 4,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };
        let mut donor = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut donor, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let original_ref = plan.root_ref.expect("tiny PDF catalog");
        let new_ref = renumber
            .new_for_original(original_ref)
            .expect("catalog is in the linearization map");
        let object = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (
                    b"/Label".to_vec(),
                    ObjectHandle::string(b"linearized label".to_vec()),
                ),
                (b"/Length".to_vec(), ObjectHandle::integer(7)),
            ]),
            std::rc::Rc::new(b"payload".to_vec()),
        );
        let options = WriterOptions {
            compress_streams: crate::writer::CompressStreams::No,
            ..WriterOptions::default()
        };
        let mut emitter = EncryptedStringEmitter::from_context(&context);
        let mut bytes = Vec::new();

        let offset = append_body_object(
            &mut bytes,
            new_ref,
            original_ref,
            &object,
            &options,
            Some(&context),
            Some(&mut emitter),
            &renumber,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .expect("linearized encrypted body stream");

        assert_eq!(offset, 0);
        assert!(bytes
            .windows(b"\nstream\n".len())
            .any(|w| w == b"\nstream\n"));
        assert!(bytes.ends_with(b"\nendobj\n"));
        assert!(!bytes
            .windows(b"linearized label".len())
            .any(|w| w == b"linearized label"));
    }

    #[test]
    fn prepend_crypt_filter_to_handle_entries_preserves_qpdf_filter_shapes() -> Result<()> {
        fn rewritten_dictionary(entries: Vec<(Vec<u8>, ObjectHandle)>) -> ObjectHandle {
            let mut entries = entries.into_iter().collect();
            prepend_crypt_filter_to_handle_entries(&mut entries, b"Identity")
                .expect("direct filter-shape handles must be writable");
            ObjectHandle::dictionary(entries.into_iter().collect())
        }

        let no_filter = rewritten_dictionary(Vec::new());
        assert_eq!(
            no_filter.try_get_key(b"/Filter")?.try_as_name()?.as_deref(),
            Some(b"Crypt".as_slice())
        );

        let array_without_decode_parms = rewritten_dictionary(vec![(
            b"/Filter".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::name(b"FlateDecode".to_vec()),
                ObjectHandle::name(b"ASCII85Decode".to_vec()),
            ]),
        )]);
        let filters = array_without_decode_parms
            .try_get_key(b"/Filter")?
            .try_as_array()?
            .expect("array filter chain");
        assert_eq!(filters.len(), 3);
        assert_eq!(
            filters[0].try_as_name()?.as_deref(),
            Some(b"Crypt".as_slice())
        );
        let decode_parms = array_without_decode_parms
            .try_get_key(b"/DecodeParms")?
            .try_as_array()?
            .expect("array decode-parameter chain");
        assert_eq!(decode_parms.len(), 3);
        assert!(decode_parms[1].try_is_null()?);
        assert!(decode_parms[2].try_is_null()?);

        let existing_params =
            ObjectHandle::dictionary(vec![(b"/Predictor".to_vec(), ObjectHandle::integer(12))]);
        let array_with_array_decode_parms = rewritten_dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"ASCII85Decode".to_vec()),
                ]),
            ),
            (
                b"/DecodeParms".to_vec(),
                ObjectHandle::array(vec![existing_params.clone()]),
            ),
        ]);
        let decode_parms = array_with_array_decode_parms
            .try_get_key(b"/DecodeParms")?
            .try_as_array()?
            .expect("array decode-parameter chain");
        assert_eq!(decode_parms.len(), 3);
        assert_eq!(
            decode_parms[1].try_get_key(b"/Predictor")?.as_integer(),
            Some(12)
        );
        assert!(decode_parms[2].try_is_null()?);

        let array_with_scalar_decode_parms = rewritten_dictionary(vec![
            (
                b"/Filter".to_vec(),
                ObjectHandle::array(vec![
                    ObjectHandle::name(b"FlateDecode".to_vec()),
                    ObjectHandle::name(b"ASCII85Decode".to_vec()),
                ]),
            ),
            (b"/DecodeParms".to_vec(), existing_params),
        ]);
        let decode_parms = array_with_scalar_decode_parms
            .try_get_key(b"/DecodeParms")?
            .try_as_array()?
            .expect("array decode-parameter chain");
        assert_eq!(decode_parms.len(), 3);
        assert_eq!(
            decode_parms[1].try_get_key(b"/Predictor")?.as_integer(),
            Some(12)
        );
        assert!(decode_parms[2].try_is_null()?);

        let malformed_filter =
            rewritten_dictionary(vec![(b"/Filter".to_vec(), ObjectHandle::integer(7))]);
        assert_eq!(
            malformed_filter
                .try_get_key(b"/Filter")?
                .try_as_name()?
                .as_deref(),
            Some(b"Crypt".as_slice())
        );
        assert_eq!(
            malformed_filter
                .try_get_key(b"/DecodeParms")?
                .try_get_key(b"/Name")?
                .try_as_name()?
                .as_deref(),
            Some(b"Identity".as_slice())
        );
        Ok(())
    }

    #[test]
    fn append_body_object_emits_plain_stream_without_legacy_materialization() {
        let mut donor = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut donor, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let original_ref = plan.root_ref.expect("tiny PDF catalog");
        let new_ref = renumber
            .new_for_original(original_ref)
            .expect("catalog is in the linearization map");
        let object = ObjectHandle::stream(
            ObjectHandle::dictionary(vec![
                (
                    b"/Label".to_vec(),
                    ObjectHandle::string(b"plain linearized label".to_vec()),
                ),
                (b"/Length".to_vec(), ObjectHandle::integer(7)),
            ]),
            std::rc::Rc::new(b"payload".to_vec()),
        );
        let options = WriterOptions {
            compress_streams: crate::writer::CompressStreams::No,
            ..WriterOptions::default()
        };
        let mut bytes = Vec::new();

        let offset = append_body_object(
            &mut bytes,
            new_ref,
            original_ref,
            &object,
            &options,
            None,
            None,
            &renumber,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .expect("linearized plain body stream");

        assert_eq!(offset, 0);
        assert!(bytes
            .windows(b"/Label (plain linearized label)".len())
            .any(|w| { w == b"/Label (plain linearized label)" }));
        assert!(bytes
            .windows(b"stream\npayloadendstream".len())
            .any(|w| { w == b"stream\npayloadendstream" }));
        assert!(bytes.ends_with(b"\nendobj\n"));
    }

    #[test]
    fn append_body_object_reports_missing_reference_renumbering() {
        let mut donor = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut donor, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let original_ref = plan.root_ref.expect("tiny PDF catalog");
        let object = ObjectHandle::dictionary(vec![(
            b"/Ref".to_vec(),
            ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(
                ObjectRef::new(999, 0),
            )),
        )]);
        let error = append_body_object(
            &mut Vec::new(),
            renumber
                .new_for_original(original_ref)
                .expect("catalog is in the linearization map"),
            original_ref,
            &object,
            &WriterOptions::default(),
            None,
            None,
            &renumber,
            &BTreeSet::new(),
            &BTreeSet::new(),
        )
        .expect_err("missing reference must not be silently emitted");
        assert!(error.to_string().contains("has no renumber entry"));
    }

    #[test]
    fn append_objstm_container_reports_missing_reference_renumbering() {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        pdf.get_object_handle(ObjectRef::new(1, 0))
            .replace_key(
                b"/Ghost",
                ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(
                    ObjectRef::new(999, 0),
                )),
            )
            .expect("test-only catalog mutation");
        let container = ObjStmContainer {
            container_new_num: 10,
            members: vec![(ObjectRef::new(1, 0), ObjectRef::new(1, 0))],
        };
        let error = append_objstm_container_object(
            &mut Vec::new(),
            &container,
            &renumber,
            &mut pdf,
            &BTreeSet::new(),
            false,
            None,
        )
        .expect_err("missing nested reference must not be silently emitted");
        assert!(error.to_string().contains("has no renumber entry"));
    }

    #[test]
    fn append_objstm_container_converts_stream_members_to_null() {
        let mut pdf = Pdf::open(Cursor::new(
            include_bytes!("../../../../tests/fixtures/compat/three-page.pdf").to_vec(),
        ))
        .expect("three-page fixture should parse");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let original_ref = ObjectRef::new(9, 0);
        let new_ref = renumber
            .new_for_original(original_ref)
            .expect("page-1 content stream must be reachable");
        let container = ObjStmContainer {
            container_new_num: 20,
            members: vec![(original_ref, new_ref)],
        };
        let mut bytes = Vec::new();

        append_objstm_container_object(
            &mut bytes,
            &container,
            &renumber,
            &mut pdf,
            &BTreeSet::new(),
            false,
            None,
        )
        .expect("stream members must be represented as null in ObjStm bodies");

        assert!(
            bytes.windows(b"null".len()).any(|window| window == b"null"),
            "a malformed ObjStm stream member must be emitted as null"
        );
    }

    #[test]
    fn append_objstm_container_propagates_encryption_pipeline_errors() {
        use crate::writer::{EncryptionContext, WriteCipher};

        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let original_ref = plan.root_ref.expect("tiny PDF catalog");
        let new_ref = renumber
            .new_for_original(original_ref)
            .expect("catalog must be reachable");
        let container = ObjStmContainer {
            container_new_num: 20,
            members: vec![(original_ref, new_ref)],
        };
        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 31],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(99, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };

        let error = append_objstm_container_object(
            &mut Vec::new(),
            &container,
            &renumber,
            &mut pdf,
            &BTreeSet::new(),
            false,
            Some(&context),
        )
        .expect_err("invalid AES key material must propagate from the pipeline");
        assert!(error.to_string().contains("AES"));
    }

    #[test]
    fn canonical_linearization_trailer_entries_uses_live_values_and_filters_removed() {
        let removed_ref = ObjectRef::new(3, 0);
        let trailer = ObjectHandle::dictionary(vec![
            (b"/Foo".to_vec(), ObjectHandle::integer(7)),
            (
                b"/Ref".to_vec(),
                ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(
                    ObjectRef::new(2, 0),
                )),
            ),
            (
                b"/Removed".to_vec(),
                ObjectHandle::from_value(crate::object_handle::ObjectValue::Reference(removed_ref)),
            ),
            (b"/Null".to_vec(), ObjectHandle::null()),
            (b"/Size".to_vec(), ObjectHandle::integer(99)),
        ]);
        let map = |object_ref: ObjectRef| {
            Ok(ObjectRef::new(
                object_ref.number + 10,
                object_ref.generation,
            ))
        };

        let entries =
            canonical_linearization_trailer_entries(&trailer, &map, &BTreeSet::from([removed_ref]))
                .expect("live trailer values should serialize");

        assert_eq!(
            entries,
            vec![
                (b"/Foo".to_vec(), b"7".to_vec()),
                (b"/Ref".to_vec(), b"12 0 R".to_vec()),
            ]
        );
    }

    /// A live, non-writer-owned trailer entry that is itself an indirect
    /// handle must serialize as the mapped `"N 0 R"` reference token, never
    /// as the dereferenced object body. qpdf's `writeTrailer` unparses every
    /// surviving trailer key through `unparseChild`
    /// (`QPDFWriter.cc:1143-1155`), which branches only on
    /// `child.isIndirect()` and never inspects what the reference resolves
    /// to -- so a stream target must stay a bare reference, not collapse to
    /// its stream dictionary (losing the stream body entirely, since the
    /// generic handle-graph unparse used for *direct* values never emits
    /// `stream`/`endstream` framing for an indirect child it did not take
    /// this branch to reach). Confirmed against real qpdf 11.9.0
    /// (`qpdf --linearize --object-streams=generate`), which emits
    /// `/CustomTrailer 2 0 R` for the equivalent input, never an inlined
    /// dictionary.
    #[test]
    fn canonical_linearization_trailer_entries_preserves_indirect_stream_reference() {
        let mut pdf = open_tiny_pdf_with_custom_trailer_stream();
        let trailer = pdf.trailer();
        let map = |object_ref: ObjectRef| {
            Ok(ObjectRef::new(
                object_ref.number + 100,
                object_ref.generation,
            ))
        };

        let entries = canonical_linearization_trailer_entries(&trailer, &map, &BTreeSet::new())
            .expect("live trailer values should serialize");

        let custom = entries
            .iter()
            .find(|(key, _)| key == b"/CustomTrailer")
            .map(|(_, value)| value.clone())
            .expect("/CustomTrailer entry must survive trimming");
        assert_eq!(
            custom, b"104 0 R",
            "an indirect trailer value naming a stream must stay a mapped \
             reference, matching qpdf's unparseChild rather than inlining \
             the dereferenced stream dictionary"
        );
    }

    /// A custom trailer literal string may contain the exact deterministic
    /// `/ID` placeholder bytes. The xref-stream patch must replace only the
    /// actual `/ID` tokens and preserve that user value. This is the concrete
    /// regression for the former whole-section scan in
    /// `patch_linearized_deterministic_id`.
    #[test]
    fn deterministic_id_objstm_survives_custom_trailer_placeholder_lookalike() {
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
        pdf.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
            )
            .as_bytes(),
        );
        // Literal string content embeds the exact 70-byte placeholder run a
        // default (no source /ID) deterministic-id save would install:
        // `[` + 32 '0' + `><` + 32 '0' + `>]`. None of `[`, `<`, `0`, `>`,
        // `]` are literal-string-special, so `write_literal_string` emits
        // them verbatim.
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size 4 /Root 1 0 R /CustomTrailer \
                 (POISON[<{}><{}>]POISON) >>\nstartxref\n{xref_start}\n%%EOF\n",
                "0".repeat(32),
                "0".repeat(32),
            )
            .as_bytes(),
        );

        let mut src = Pdf::open(Cursor::new(pdf.clone())).expect("probe fixture must parse");
        let plan = LinearizationPlan::from_pdf(&mut src, true).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let opts = WriterOptions {
            deterministic_id: true,
            object_streams: crate::writer::ObjectStreamMode::Generate,
            ..WriterOptions::default()
        };
        let mut src2 = Pdf::open(Cursor::new(pdf)).expect("probe fixture must reparse");
        let mut doc = write_linearized(&plan, &renumber, &mut src2, &opts)
            .expect("custom trailer placeholder must not trip the ID patcher");
        doc.back_patch().expect("back_patch");

        let placeholder = b"[<00000000000000000000000000000000><00000000000000000000000000000000>]";
        let occurrences = doc
            .bytes
            .windows(placeholder.len())
            .filter(|w| *w == &placeholder[..])
            .count();
        assert_eq!(
            occurrences, 1,
            "the one placeholder-shaped user literal must survive while both xref /ID tokens are patched"
        );

        let id_marker = b"/ID [";
        assert_eq!(
            doc.bytes
                .windows(id_marker.len())
                .filter(|w| *w == id_marker)
                .count(),
            2,
            "first-page and main xref streams must each retain one /ID array"
        );
    }

    #[test]
    fn id_helpers_reject_noncanonical_shapes() {
        assert!(id_object_to_handle(&Object::Null).is_err());
        assert!(id_object_to_handle(&Object::Array(vec![Object::Null])).is_err());
        assert!(id_object_to_handle(&Object::Array(vec![
            Object::String(b"id".to_vec()),
            Object::Integer(7),
        ]))
        .is_err());

        let missing = ObjectHandle::dictionary(Vec::new());
        assert!(xref_id_bytes(&missing)
            .expect("missing ID is a valid optional shape")
            .is_none());
        let one = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::string(b"id".to_vec())]),
        )]);
        assert!(xref_id_bytes(&one)
            .expect("short ID array is an optional shape")
            .is_none());
        let wrong_type = ObjectHandle::dictionary(vec![(
            b"/ID".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::string(b"id".to_vec()),
                ObjectHandle::integer(7),
            ]),
        )]);
        assert!(xref_id_bytes(&wrong_type)
            .expect("wrong ID element types are an optional shape")
            .is_none());
    }

    /// Task 8: the hint stream is encrypted like any other stream payload —
    /// qpdf's `writeHintStream` calls `setDataKey(hint_id)` before writing
    /// the dict/payload (QPDFWriter.cc:2297), unlike the xref table/stream
    /// and the `/Encrypt` dict, which stay plaintext unconditionally.
    /// Constructs an `EncryptionContext` directly (mirroring
    /// `crate::writer::tests::rc4_stream_encryption_preserves_payload_allocation`)
    /// to exercise `append_hint_stream_object` in isolation.
    #[test]
    fn append_hint_stream_object_encrypts_payload_when_ctx_present() {
        use crate::writer::{EncryptionContext, WriteCipher};

        let payload = b"page offset hint table + shared object hint table payload".to_vec();
        let object_ref = ObjectRef::new(9, 0);
        let ctx = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 16],
            cipher: WriteCipher::PerObject(crate::ObjectKeyAlg::Aes),
            encryption_v: 4,
            encryption_r: 4,
            encrypt_ref: ObjectRef::new(2, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };

        // Independently run the canonical writer pipeline with the same
        // explicit IV. This is the oracle for what append_hint_stream_object
        // must embed, without routing the test through the legacy buffer
        // replacement helper.
        let mut expected_object = Vec::new();
        crate::writer::write_stream_payload_with_pipeline_qdf(
            &mut expected_object,
            &payload,
            NewlineBeforeEndstream::Never,
            true,
            object_ref,
            &ctx,
            true,
            Some(crate::pipeline::aes::static_initialization_vector()),
        )
        .expect("compute expected ciphertext");
        let stream_marker = b"\nstream\n";
        let payload_start = expected_object
            .windows(stream_marker.len())
            .position(|window| window == stream_marker)
            .expect("canonical pipeline must emit stream framing")
            + stream_marker.len();
        let mut expected_payload_len = payload.len();
        crate::writer::adjust_aes_stream_length(&mut expected_payload_len, &ctx, true)
            .expect("expected encrypted length must fit");
        let expected_ciphertext =
            expected_object[payload_start..payload_start + expected_payload_len].to_vec();

        let mut bytes = Vec::new();
        let offset = append_hint_stream_object(
            &mut bytes,
            object_ref,
            &payload,
            46,
            None,
            false,
            Some(&ctx),
            crate::pipeline::aes::static_initialization_vector(),
        )
        .expect("emit with encrypt_ctx");
        assert_eq!(offset, 0);

        assert!(
            !bytes
                .windows(payload.len())
                .any(|w| w == payload.as_slice()),
            "hint stream payload must not appear in plaintext when encrypt_ctx is Some"
        );
        assert!(
            bytes
                .windows(expected_ciphertext.len())
                .any(|w| w == expected_ciphertext.as_slice()),
            "hint stream must embed the same ciphertext the canonical writer \
             pipeline produces for this object ref + key material"
        );

        let length_marker = format!("/Length {}", expected_ciphertext.len());
        assert!(
            String::from_utf8_lossy(&bytes).contains(&length_marker),
            "/Length must reflect the encrypted (not plaintext) byte count"
        );
    }

    #[test]
    fn append_hint_stream_object_propagates_encryption_pipeline_errors() {
        use crate::writer::{EncryptionContext, WriteCipher};

        let context = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 31],
            cipher: WriteCipher::FileKeyAes256,
            encryption_v: 5,
            encryption_r: 6,
            encrypt_ref: ObjectRef::new(2, 0),
            id0: Vec::new(),
            static_aes_iv: true,
            encrypt_metadata: true,
            metadata_ref: None,
        };

        let error = append_hint_stream_object(
            &mut Vec::new(),
            ObjectRef::new(9, 0),
            b"hint payload",
            46,
            None,
            false,
            Some(&context),
            crate::pipeline::aes::static_initialization_vector(),
        )
        .expect_err("invalid AES key material must propagate from the pipeline");
        assert!(error.to_string().contains("AES"));
    }

    /// Proves the mechanism the PR-review-flagged bug depends on: encrypting
    /// the SAME plaintext hint-stream payload with two DIFFERENT AES IVs can
    /// produce hint-stream *objects* of different total byte length, because
    /// [`append_hint_stream_object`]'s newline-before-`endstream` decision
    /// (qpdf, QPDFWriter.cc:2327) is data-dependent on the CIPHERTEXT's last
    /// byte, and AES-CBC ciphertext is a function of the IV as well as the
    /// plaintext (each ciphertext block chains through the previous one,
    /// starting from the IV).
    ///
    /// `IV_NO_NEWLINE` / `IV_WITH_NEWLINE` were found by brute-forcing 16-byte
    /// IVs against this exact payload + key until one landed on each side of
    /// the boundary (`stream.data.last() == Some(&b'\n')`); they are not
    /// otherwise meaningful values. This direct fixed-IV framing test covers
    /// the local ciphertext/newline length difference. The deterministic
    /// end-to-end test
    /// [`deterministic_encrypted_hint_cases_cover_both_ciphertext_framing_outcomes`]
    /// below covers both outcomes through the complete linearization pipeline,
    /// while the separate random-IV test retains production-path coverage and
    /// checks the resulting Shared Objects and Outlines offsets. The
    /// `append_hint_stream_object` doc describes the framing invariant that
    /// connects these tests.
    #[test]
    fn identical_plaintext_different_iv_can_change_hint_stream_object_length() {
        use crate::writer::{EncryptionContext, WriteCipher};

        const IV_NO_NEWLINE: [u8; 16] = [0u8; 16];
        const IV_WITH_NEWLINE: [u8; 16] = [146, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

        let payload = b"page offset hint table + shared object hint table payload".to_vec();
        let object_ref = ObjectRef::new(9, 0);
        let ctx = EncryptionContext {
            encrypt_dict: Dictionary::new(),
            file_key: vec![0x11; 16],
            cipher: WriteCipher::PerObject(crate::ObjectKeyAlg::Aes),
            encryption_v: 4,
            encryption_r: 4,
            encrypt_ref: ObjectRef::new(2, 0),
            id0: Vec::new(),
            static_aes_iv: false, // irrelevant here: the IV is passed explicitly
            encrypt_metadata: true,
            metadata_ref: None,
        };

        let emit = |iv: [u8; 16]| -> Vec<u8> {
            let mut bytes = Vec::new();
            append_hint_stream_object(
                &mut bytes,
                object_ref,
                &payload,
                46,
                None,
                false,
                Some(&ctx),
                iv,
            )
            .expect("emit with encrypt_ctx");
            bytes
        };

        let with_no_newline_iv = emit(IV_NO_NEWLINE);
        let with_newline_iv = emit(IV_WITH_NEWLINE);

        // Sanity: confirm the brute-forced IVs actually land on opposite
        // sides of the ciphertext-last-byte boundary, independently of the
        // object-length assertion below (a passing length assertion for the
        // wrong reason would be worse than no test).
        let payload_len_from = |ctx: &EncryptionContext, iv: [u8; 16]| -> usize {
            let mut object = Vec::new();
            crate::writer::write_stream_payload_with_pipeline_qdf(
                &mut object,
                &payload,
                NewlineBeforeEndstream::Never,
                true,
                object_ref,
                ctx,
                true,
                Some(iv),
            )
            .unwrap();
            object.len()
        };
        assert_ne!(
            payload_len_from(&ctx, IV_NO_NEWLINE),
            payload_len_from(&ctx, IV_WITH_NEWLINE),
            "test premise: the two brute-forced IVs must land on opposite \
             sides of the newline-before-endstream boundary for this payload"
        );

        assert_ne!(
            with_no_newline_iv.len(),
            with_newline_iv.len(),
            "identical plaintext through two different IVs must be able to \
             produce hint-stream OBJECTS of different total byte length \
             (this is the exact premise the reviewer-flagged bug depends on)"
        );
    }

    /// qpdf's hint tables are calculated from the first write pass, where the
    /// hint object slot is reserved but emits no bytes.  The resulting offsets
    /// are already the virtual values that qpdf's `adjusted_offset` restores
    /// after the exact hint object is spliced into pass 2; they must not be
    /// corrected by subtracting a guessed hint-object length here.
    #[test]
    fn build_outline_hint_table_uses_pass1_virtual_offset() {
        let info = OutlineHintInfo {
            first_object: 3,
            nobjects: 2,
        };
        let byte_lengths = BTreeMap::from([(3u32, 60usize), (4u32, 70usize)]);

        let table =
            build_outline_hint_table(&info, &BTreeMap::from([(3u32, 500usize)]), &byte_lengths)
                .expect("pass-1 outline offset is already virtual");

        assert_eq!(table.first_object_offset, 500);
        assert_eq!(table.group_length, 130);
    }

    /// Read a `"{key}: "`-prefixed decimal field out of a
    /// [`crate::linearization::show_linearization_bytes`] dump — the same
    /// text format qpdf's `--show-linearization` produces. Exact line-prefix
    /// match (not substring search), so e.g. `"first_object: "` never matches
    /// the unrelated `"first_page_object: "` line.
    fn parse_dump_field(dump: &str, key: &str) -> u64 {
        let needle = format!("{key}: ");
        for line in dump.lines() {
            if let Some(rest) = line.strip_prefix(&needle) {
                // cov:ignore-start: defensive — show_linearization_bytes's
                // dump_* helpers always write these fields as plain decimal
                // via `{}` on a numeric value, so `rest` is always parseable
                // for a well-formed dump; this only guards a future format
                // change in show.rs from failing silently instead of loudly.
                return rest
                    .trim()
                    .parse()
                    .unwrap_or_else(|e| panic!("field {key:?} = {rest:?} is not decimal: {e}"));
                // cov:ignore-end
            }
        }
        panic!("dump has no \"{needle}\" line:\n{dump}"); // cov:ignore: defensive — every dump this test module produces contains every field it queries; only guards a future show.rs field-name rename from failing silently.
    }

    /// End-to-end proof: a real linearized + AES-128-encrypted document
    /// carrying BOTH an Outlines Hint Table entry and a Part-8
    /// (`part4_other_pages_shared`) Shared Objects Hint Table entry — the two
    /// concrete tables the PR review named — has internally self-consistent
    /// hint tables with a genuinely random (non-`--static-aes-iv`) IV. The
    /// deterministic companion test below supplies the complementary proof
    /// that both ciphertext-dependent framing outcomes work end to end.
    ///
    /// Decode the hint stream via
    /// [`crate::linearization::show_linearization_bytes`] (the same decoder
    /// that reconstructs qpdf's `adjusted_offset`, i.e. `stored_value +
    /// /H[1]`) and independently locate the REAL physical byte offset of the
    /// referenced objects by scanning the actually-shipped bytes — then assert
    /// they agree. `check_linearization_bytes` alone would not catch a
    /// regression here: per its own doc table it validates the linearization
    /// PARAMETER DICT (`/L /N /O /H /E /T`), not the hint stream's internal
    /// Page/Shared/Outline tables — this test's `show_linearization_bytes` +
    /// manual offset reconstruction is the actual oracle for those.
    #[test]
    fn linearized_encrypted_outline_and_part8_shared_hint_tables_are_consistent_with_random_iv() {
        let src = outlines_and_part8_shared_pdf_bytes();
        let out = linearize_with(&src, |o| {
            // Empty user password so `check_linearization_bytes` and
            // `show_linearization_bytes` below (both open with no password)
            // can decrypt transparently — the same convention
            // `linearize_with_encrypt_body_strings_and_streams_are_ciphertext`
            // uses. `static_aes_iv` stays at its default `false`, so this
            // case uses a genuinely random IV.
            o.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
                Vec::new(),
                b"owner".to_vec(),
            ));
        });

        crate::linearization::check_linearization_bytes(&out)
            .expect("encrypted linearized output must pass the linearization checker");
        assert_encrypted_body_strings_are_hex(&out);

        let dump = crate::linearization::show_linearization_bytes(&out, "test")
            .expect("hint stream must decode (decryption + bit-unpacking)");

        // Shared Objects Hint Table: `first_shared_offset` (already
        // adjusted_offset()-reconstructed by the dump) must equal the REAL
        // physical offset of the renumbered first Part-8 object.
        let first_shared_obj = parse_dump_field(&dump, "first_shared_obj") as u32;
        let first_shared_offset = parse_dump_field(&dump, "first_shared_offset") as usize;
        let real_shared_offset = find_object_offset(&out, first_shared_obj);
        assert_eq!(
            first_shared_offset, real_shared_offset,
            "Shared Objects Hint Table's first_shared_offset must match the \
             real physical offset of object {first_shared_obj} (dump:\n{dump})"
        );

        // Outlines Hint Table: same check for `first_object_offset`.
        assert!(
            dump.contains("Outlines Hint Table"),
            "test premise: fixture's /Outlines must produce an Outlines Hint \
             Table section (dump:\n{dump})"
        );
        let first_object = parse_dump_field(&dump, "first_object") as u32;
        let first_object_offset = parse_dump_field(&dump, "first_object_offset") as usize;
        let real_object_offset = find_object_offset(&out, first_object);
        assert_eq!(
            first_object_offset, real_object_offset,
            "Outlines Hint Table's first_object_offset must match the real \
             physical offset of object {first_object} (dump:\n{dump})"
        );
    }

    /// Deterministic end-to-end proof of both ciphertext-dependent hint-stream
    /// framing outcomes. The same three-page graph, Outlines root, and Part-8
    /// shared object is encrypted twice with fixed IVs selected to land on
    /// opposite ciphertext-last-byte outcomes.
    ///
    /// Each output goes through the complete `linearize_with` pipeline, then
    /// checks the linearization parameter dictionary, encrypted body strings,
    /// decrypted hint tables, and the Shared Objects/Outlines offsets against
    /// independently located object headers. The framing assertion is based
    /// on the declared encrypted `/Length`, so it does not mistake qpdf's
    /// optional framing newline for ciphertext.
    ///
    /// The first invocation deliberately arms the test-only IV queue with the
    /// probe/final pair. The current qpdf-shaped implementation must consume
    /// exactly one IV while building the complete hint object and replay that
    /// object in pass 2; if a probe/final re-encryption loop returns, it
    /// consumes the second, opposite-framing IV and this assertion fails.
    #[test]
    fn deterministic_encrypted_hint_cases_cover_both_ciphertext_framing_outcomes() {
        let source = outlines_and_part8_shared_pdf_bytes();
        let hint_stream_num = encrypted_hint_stream_number(&source);
        let (first, remaining) = with_test_hint_stream_aes_ivs(
            [HINT_IV_NO_FRAMING_NEWLINE, HINT_IV_WITH_FRAMING_NEWLINE],
            || linearize_with(&source, configure_deterministic_aes128),
        );
        assert_eq!(
            remaining,
            vec![HINT_IV_WITH_FRAMING_NEWLINE],
            "the complete hint object must be encrypted once; a second queued IV \
             would indicate probe/final re-encryption"
        );
        let (second, remaining) =
            with_test_hint_stream_aes_ivs([HINT_IV_WITH_FRAMING_NEWLINE], || {
                linearize_with(&source, configure_deterministic_aes128)
            });
        assert!(remaining.is_empty());

        let mut framing_newline = Vec::new();
        for (label, output) in [("no-framing-newline", &first), ("framing-newline", &second)] {
            crate::linearization::check_linearization_bytes(output).unwrap_or_else(|error| {
                // cov:ignore-start: the test constructs valid linearized PDFs;
                // this branch only guards an unexpected checker regression.
                panic!("{label}: encrypted linearized output must pass checker: {error}")
                // cov:ignore-end
            });
            assert_encrypted_body_strings_are_hex(output);

            let dump = crate::linearization::show_linearization_bytes(output, "test")
                .unwrap_or_else(|error| panic!("{label}: hint stream must decode: {error}"));

            let first_shared_obj = parse_dump_field(&dump, "first_shared_obj") as u32;
            let first_shared_offset = parse_dump_field(&dump, "first_shared_offset") as usize;
            assert_eq!(
                first_shared_offset,
                find_object_offset(output, first_shared_obj),
                "{label}: Shared Objects Hint Table offset must match object {first_shared_obj}"
            );

            assert!(
                dump.contains("Outlines Hint Table"),
                "{label}: fixture must produce an Outlines Hint Table (dump:\n{dump})"
            );
            let first_object = parse_dump_field(&dump, "first_object") as u32;
            let first_object_offset = parse_dump_field(&dump, "first_object_offset") as usize;
            assert_eq!(
                first_object_offset,
                find_object_offset(output, first_object),
                "{label}: Outlines Hint Table offset must match object {first_object}"
            );

            let payload = hint_stream_payload_bytes(output, hint_stream_num);
            let has_framing_newline =
                hint_stream_has_newline_before_endstream(output, hint_stream_num);
            assert_eq!(
                payload.last() == Some(&b'\n'),
                !has_framing_newline,
                "{label}: framing newline must be the inverse of the ciphertext final byte"
            );
            framing_newline.push(has_framing_newline);
        }

        assert_eq!(
            framing_newline,
            [false, true],
            "the deterministic end-to-end cases must cover both hint-stream framing outcomes"
        );
    }

    /// Extract just the hint stream object's stream *payload* using the
    /// dictionary's `/Length`, locating the object by its
    /// pre-computed reserved number. Using `/Length` rather than the next
    /// `endstream` marker is essential: qpdf conditionally emits one framing
    /// newline after a payload whose last byte is not `\n`, and that newline is
    /// not part of the ciphertext.
    fn hint_stream_payload_bytes(out: &[u8], hint_num: u32) -> &[u8] {
        let object_bytes = find_object_bytes(out, hint_num);
        let stream_marker: &[u8] = b"stream\n";
        let payload_start = object_bytes
            .windows(stream_marker.len())
            .position(|w| w == stream_marker)
            .unwrap_or_else(|| {
                // cov:ignore-start: only reached if the hint stream object
                // has no "stream\n" marker — every well-formed fixture in
                // this test module reaches the assertion under test instead.
                panic!("hint stream object {hint_num} must contain a \"stream\\n\" marker")
                // cov:ignore-end
            })
            + stream_marker.len();
        let length_marker: &[u8] = b"/Length ";
        let length_start = object_bytes
            .windows(length_marker.len())
            .position(|w| w == length_marker)
            .unwrap_or_else(|| {
                // cov:ignore-start: every primary hint-stream dictionary
                // carries /Length; this only guards a future serializer drift.
                panic!("hint stream object {hint_num} must contain /Length")
                // cov:ignore-end
            })
            + length_marker.len();
        let length_end = object_bytes[length_start..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .map(|offset| length_start + offset)
            .unwrap_or(object_bytes.len());
        let payload_len: usize = std::str::from_utf8(&object_bytes[length_start..length_end])
            .unwrap_or_else(|error| {
                // cov:ignore-start: /Length is emitted as ASCII decimal.
                panic!("hint stream /Length is not ASCII: {error}")
                // cov:ignore-end
            })
            .parse()
            .unwrap_or_else(|error| {
                // cov:ignore-start: /Length is emitted as a non-negative decimal.
                panic!("hint stream /Length is not decimal: {error}")
                // cov:ignore-end
            });
        &object_bytes[payload_start..payload_start + payload_len]
    }

    /// Return whether qpdf's conditional newline was emitted between the
    /// ciphertext payload and `endstream`. The payload itself is located by
    /// `/Length`, so this distinguishes the two framing outcomes even though
    /// both valid encodings place a newline immediately before `endstream`.
    fn hint_stream_has_newline_before_endstream(out: &[u8], hint_num: u32) -> bool {
        let object_bytes = find_object_bytes(out, hint_num);
        let stream_marker: &[u8] = b"stream\n";
        let payload_start = object_bytes
            .windows(stream_marker.len())
            .position(|window| window == stream_marker)
            .unwrap_or_else(|| {
                // cov:ignore-start: every primary hint-stream dictionary
                // carries a stream marker; this only guards serializer drift.
                panic!("hint stream object {hint_num} must contain a stream marker")
                // cov:ignore-end
            })
            + stream_marker.len();
        let payload = hint_stream_payload_bytes(out, hint_num);
        object_bytes[payload_start + payload.len()] == b'\n'
    }

    /// Locate the classic (stream-free) `xref\n<start> <count>\n` subsection
    /// header nearest the start of `out` and confirm every one of its
    /// `count` fixed-width entries is qpdf's plaintext ASCII
    /// `%010d %05d %s \n` form (`write_part1_xref_and_trailer` /
    /// `patch_part1_xref`'s `CLASSIC_XREF_ENTRY_WIDTH`-byte row) — parseable
    /// by a reader without deriving the file key first, unlike every other
    /// body object and the hint stream.
    fn assert_classic_xref_section_entries_are_ascii_plaintext(out: &[u8]) {
        let marker: &[u8] = b"\nxref\n";
        let marker_pos = out
            .windows(marker.len())
            .position(|w| w == marker)
            .unwrap_or_else(|| {
                // cov:ignore-start: only reached if the classic layout has
                // no xref section at all — the state under test always has
                // one (the Part-1 first-page subsection).
                panic!("expected a classic \"xref\\n\" section header in output")
                // cov:ignore-end
            });
        let header_start = marker_pos + marker.len();
        let header_len = out[header_start..]
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or_else(|| {
                // cov:ignore-start: only reached if the header line never
                // terminates — never true for a well-formed classic layout.
                panic!("xref subsection header must end in a newline")
                // cov:ignore-end
            });
        let header = std::str::from_utf8(&out[header_start..header_start + header_len])
            .expect("xref subsection header bytes are ASCII digits/spaces by construction");
        let mut parts = header.split(' ');
        let _start_num: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .expect("xref subsection header's first field must be a decimal object number");
        let count: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .expect("xref subsection header's second field must be a decimal entry count");
        assert!(
            count > 0,
            "xref subsection must cover at least one entry, got header {header:?} \
             (a zero count would make every ASCII assertion below vacuous)"
        );

        let entries_start = header_start + header_len + 1;
        for i in 0..count as usize {
            let start = entries_start + i * CLASSIC_XREF_ENTRY_WIDTH;
            let entry = std::str::from_utf8(&out[start..start + CLASSIC_XREF_ENTRY_WIDTH])
                .unwrap_or_else(|_| {
                    // cov:ignore-start: only reached if a classic xref entry
                    // is not valid ASCII — the property under test.
                    panic!("xref entry {i} must be ASCII, got non-UTF8 bytes")
                    // cov:ignore-end
                });
            let fields: Vec<&str> = entry.trim_end_matches('\n').split(' ').collect();
            assert_eq!(
                fields.len(),
                4,
                "xref entry must be \"%010d %05d %s \" (trailing space before \
                 the newline this loop already stripped), got {entry:?}"
            );
            assert!(
                fields[0].len() == 10 && fields[0].bytes().all(|b| b.is_ascii_digit()),
                "xref offset field must be 10 ASCII digits, got {entry:?}"
            );
            assert!(
                fields[1].len() == 5 && fields[1].bytes().all(|b| b.is_ascii_digit()),
                "xref generation field must be 5 ASCII digits, got {entry:?}"
            );
            assert!(
                fields[2] == "n" || fields[2] == "f",
                "xref type field must be 'n' or 'f', got {entry:?}"
            );
        }
    }

    /// Task 11 qualitative check (`bd show flpdf-txag` acceptance criteria
    /// item 6): confirms the hint stream is genuinely ciphertext THROUGH THE
    /// FULL `linearize_with` pipeline — renumbering, plan, and the
    /// two-pass layout all included — not merely at the level of
    /// `append_hint_stream_object` called in isolation with a hand-built
    /// `EncryptionContext`, which
    /// [`append_hint_stream_object_encrypts_payload_when_ctx_present`]
    /// already proves against an independently computed oracle. That
    /// existing test does not go through `linearize_with` at all, so it
    /// cannot catch a bug where the full pipeline fails to *wire* an
    /// `encrypt_ctx` into the hint-stream call even though the function
    /// itself encrypts correctly when given one.
    ///
    /// The discriminator is deliberately NOT "the raw payload bytes differ
    /// from an unencrypted control": with `--encrypt`,
    /// `reserve_encrypt_dict_slot` inserts one extra object and every
    /// encrypted body object grows (a 16-byte IV + CBC padding), so the
    /// PLAINTEXT hint table itself already differs between the two runs
    /// regardless of whether the hint stream is encrypted — a plain
    /// raw-byte inequality would still pass even if the `encrypt_ctx`
    /// wiring into the hint-stream call site were silently dropped
    /// (verified empirically: temporarily changing that one call site to
    /// pass `None` left a raw-inequality version of this test green).
    /// Instead this exploits that with default options the hint stream is
    /// `/Filter /FlateDecode` (`structural_streams_filtered`,
    /// `CompressStreams::Yes`): a genuinely unencrypted FlateDecode payload
    /// MUST decode as zlib, while ciphertext (AES-CBC over already-
    /// compressed bytes) essentially never does. That flips sign exactly on
    /// "was the payload actually run through the cipher", independent of
    /// any layout/offset drift between the two runs.
    ///
    /// Companion check, same output: the classic Part-1 xref subsection
    /// stays in qpdf's plaintext ASCII `%010d %05d %s \n` form even though
    /// the document is encrypted — qpdf's `writeTrailer` never calls
    /// `setDataKey` for the xref table (`m->cur_data_key.clear()` in the
    /// `xref_stream` branch), unlike the hint stream, because a reader must
    /// be able to locate every object before it can even derive the file
    /// key.
    #[test]
    fn linearize_with_encrypt_hint_stream_is_ciphertext_xref_stays_ascii_plaintext() {
        let src = tiny_pdf_bytes();

        let mut pdf = Pdf::open(Cursor::new(src.clone())).expect("source parses");
        let plan = LinearizationPlan::from_pdf(&mut pdf, false).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let unencrypted_hint_num = renumber.hint_stream_slot();
        // reserve_encrypt_dict_slot inserts the /Encrypt slot at the OLD
        // hint_stream_slot and shifts the hint stream itself to old+1 (see
        // its own doc).
        let encrypted_hint_num = unencrypted_hint_num + 1;

        let unencrypted = linearize_with(&src, |_o| {});
        let encrypted = linearize_with(&src, |o| {
            o.static_aes_iv = true;
            o.encrypt = Some(crate::encryption::EncryptParams::v4_aes128(
                Vec::new(),
                b"owner".to_vec(),
            ));
        });

        // Sanity: both located objects really are the hint stream (carry
        // the `/S ` shared-section-offset key `hint_stream_dict_prefix`
        // always emits) — guards against the `+1` shift assumption silently
        // landing on some other object if the layout ever changes.
        for (label, object_bytes) in [
            (
                "unencrypted control",
                find_object_bytes(&unencrypted, unencrypted_hint_num),
            ),
            (
                "encrypted output",
                find_object_bytes(&encrypted, encrypted_hint_num),
            ),
        ] {
            assert!(
                object_bytes.windows(3).any(|w| w == b"/S "),
                "{label}: object at the computed hint-stream number must carry \
                 the hint stream's /S key"
            );
        }

        let unencrypted_payload = hint_stream_payload_bytes(&unencrypted, unencrypted_hint_num);
        let encrypted_payload = hint_stream_payload_bytes(&encrypted, encrypted_hint_num);

        // Coarse signal only — see the doc comment above for why this alone
        // does not prove the payload was run through the cipher.
        assert_ne!(
            unencrypted_payload, encrypted_payload,
            "hint stream payload must differ between the unencrypted control \
             and the encrypted output at their respective structural positions"
        );

        // The real, falsifiable discriminator: a bare /FlateDecode dict
        // decodes the unencrypted control's payload (real zlib) but must
        // NOT decode the encrypted output's payload (AES-CBC ciphertext).
        let mut flate_dict = Dictionary::new();
        flate_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
        crate::filters::test_dictionary_api::decode_stream_data(&flate_dict, unencrypted_payload)
            .expect(
                "premise: the unencrypted control's hint stream payload must be valid \
             zlib (structural_streams_filtered is true under default options)",
            );
        assert!(
            crate::filters::test_dictionary_api::decode_stream_data(&flate_dict, encrypted_payload)
                .is_err(),
            "encrypted output's hint stream payload must NOT decode as zlib — \
             if it does, the hint stream was never actually run through the cipher"
        );

        assert_classic_xref_section_entries_are_ascii_plaintext(&encrypted);
    }

    /// The primary hint-stream object must serialize its filtered dict in
    /// qpdf's key order `/Filter /S /Length` (so `/S` precedes `/Length`), with
    /// framing byte-identical to the generic object serializer. Asserts the
    /// complete object bytes — not just the dict substring — so a newline
    /// regression in the `stream`/`endstream`/`endobj` framing is also caught.
    #[test]
    fn append_hint_stream_object_emits_qpdf_key_order() {
        let payload = vec![0u8; 53];
        let mut bytes = Vec::new();
        let offset = append_hint_stream_object(
            &mut bytes,
            ObjectRef::new(9, 0),
            &payload,
            46,
            None,
            true,
            None,
            [0u8; 16], // unused: no encrypt_ctx
        )
        .expect("no encrypt_ctx: emission cannot fail");
        assert_eq!(offset, 0, "emitter returns its start offset");

        let mut expected = Vec::new();
        expected
            .extend_from_slice(b"9 0 obj\n<< /Filter /FlateDecode /S 46 /Length 53 >>\nstream\n");
        expected.extend_from_slice(&payload);
        expected.extend_from_slice(b"\nendstream\nendobj\n");
        assert_eq!(bytes, expected, "hint-stream object framing + key order");
    }

    /// qpdf omits `/Filter` from the primary hint stream when stream
    /// compression is disabled (`--stream-data=preserve` or `uncompress`).
    #[test]
    fn append_hint_stream_object_omits_filter_for_raw_payload() {
        let payload = b"\x00\x01\n";
        let mut bytes = Vec::new();
        append_hint_stream_object(
            &mut bytes,
            ObjectRef::new(9, 0),
            payload,
            46,
            Some(51),
            false,
            None,
            [0u8; 16], // unused: no encrypt_ctx
        )
        .expect("no encrypt_ctx: emission cannot fail");

        assert_eq!(
            bytes,
            b"9 0 obj\n<< /S 46 /O 51 /Length 3 >>\nstream\n\x00\x01\nendstream\nendobj\n"
        );
    }

    // -----------------------------------------------------------------------
    // build_outline_hint_table (qpdf calculateHOutline)
    // -----------------------------------------------------------------------

    #[test]
    fn build_outline_hint_table_uses_virtual_offset_and_consecutive_lengths() {
        // first_object = 3, nobjects = 2 → group_length sums units 3 and 4.
        let info = OutlineHintInfo {
            first_object: 3,
            nobjects: 2,
        };
        let xref_offsets = BTreeMap::from([(3u32, 500usize), (4u32, 560usize)]);
        let byte_lengths = BTreeMap::from([(3u32, 60usize), (4u32, 70usize), (5u32, 999usize)]);
        let table = build_outline_hint_table(&info, &xref_offsets, &byte_lengths).unwrap();
        assert_eq!(table.first_object, 3);
        // Pass-1 offsets already omit the exact hint object that pass 2 splices.
        assert_eq!(table.first_object_offset, 500);
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
        let err = build_outline_hint_table(&info, &BTreeMap::new(), &BTreeMap::new()).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("no probed offset")),
            "expected 'no probed offset' Unsupported error, got {err:?}"
        );
    }

    #[test]
    fn build_outline_hint_table_accepts_small_virtual_offset() {
        let info = OutlineHintInfo {
            first_object: 3,
            nobjects: 1,
        };
        // A pass-1 virtual offset may be smaller than the final hint object;
        // pass 2's qpdf adjusted_offset adds that object back later.
        let xref_offsets = BTreeMap::from([(3u32, 100usize)]);
        let table = build_outline_hint_table(&info, &xref_offsets, &BTreeMap::new()).unwrap();
        assert_eq!(table.first_object_offset, 100);
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
        let table = build_outline_hint_table(&info, &xref_offsets, &byte_lengths).unwrap();
        assert_eq!(table.first_object_offset, 1000);
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
        let err = build_outline_hint_table(&info, &xref_offsets, &BTreeMap::new()).unwrap_err();
        assert!(
            matches!(err, crate::Error::Unsupported(ref m) if m.contains("exceeds the")),
            "expected 'exceeds the 32-bit ...' Unsupported error, got {err:?}"
        );
    }
}
