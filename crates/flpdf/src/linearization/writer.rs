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
use crate::linearization::plan::{ContainerPart, LinearizationPlan, RoutedObjStmBatch};
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
use crate::{ObjectHandle, ObjectRef, Pdf, Result};

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
        // stream value that cannot be a valid ObjStm member.
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

/// Byte offsets and derived values for a linearized PDF.
///
/// All values are absolute byte positions within `LinearizedDocument::bytes`
/// unless stated otherwise. These values describe the byte layout used to
/// fill the Part 1 parameter dictionary.
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
/// [`write_encryption_dictionary_handle`](crate::writer::encrypted_strings::write_encryption_dictionary_handle)
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
    id_writer: Option<crate::pdf_syntax::ReborrowableIdWriter>,
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
        crate::pdf_syntax::write_name_escaped(bytes, key.strip_prefix(b"/").unwrap_or(&key));
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
    id_writer: Option<crate::pdf_syntax::ReborrowableIdWriter>,
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
            || value.object_ref().is_some_and(|object_ref| {
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
) -> ObjectHandle {
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
        ObjectHandle::array(vec![
            ObjectHandle::string(vec![0u8; len0]),
            ObjectHandle::string(vec![0u8; 16]),
        ])
    } else if let Some(source) = copy_encryption {
        let generated = crate::writer::generate_id_handle(None, options.static_id);
        let id1 = generated
            .as_array()
            .and_then(|values| values.get(1).and_then(ObjectHandle::as_string))
            .unwrap_or_else(|| source.id0.clone());
        ObjectHandle::array(vec![
            ObjectHandle::string(source.id0.clone()),
            ObjectHandle::string(id1),
        ])
    } else {
        crate::writer::generate_id_handle(source_id0, options.static_id)
    }
}

/// Build qpdf's pass-1 `/ID` placeholder from the original trailer.
///
/// `QPDFWriter::writeTrailer` (qpdf 11.9.0, lines 1197-1213) ignores the
/// selected final-ID policy during linearization pass 1. It writes an all-zero
/// first string with the same byte width as the original `/ID[0]` (falling
/// back to 16 bytes when there is no non-empty original identifier), followed
/// by a 16-byte all-zero changing identifier.
fn linearization_pass1_id(source_id0: Option<&[u8]>) -> ObjectHandle {
    let first_len = source_id0.map(|id| id.len()).unwrap_or(16);
    ObjectHandle::array(vec![
        ObjectHandle::string(vec![0u8; first_len]),
        ObjectHandle::string(vec![0u8; 16]),
    ])
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
            root_value: None,
            size: final_size,
            prev: Some(0),
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
        root_value: None,
        size: patch.size,
        prev: Some(prev),
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
        root_value: None,
        size: main_count,
        prev: None,
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
            root_value: None,
            size: main_count,
            prev: None,
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

/// Emit the primary hint-stream object and return its start byte offset.
///
/// qpdf 11.9.0 serializes the hint-stream object dict in the key order
/// optional `/Filter`, then `/S`, `/O` when present, and `/Length` (observed
/// against its `--check-linearization` golden output), which the generic
/// `BTreeMap`-ordered stream serializer cannot reproduce. This
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
    let outlines_ref = if let Some(root_ref) = pdf.root_ref() {
        let root = pdf.get_object_handle(root_ref);
        pdf.resolve(&root)?;
        if root.try_as_dictionary()?.is_none() {
            None // cov:ignore: catalog is always a dict when outlines exist
        } else {
            let outlines = root.try_get_key(b"/Outlines")?;
            outlines.object_ref()
        }
    } else {
        None // cov:ignore: a non-empty retained outline set has a Catalog root
    };
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
    mut id_writer: Option<crate::pdf_syntax::ReborrowableIdWriter>,
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
        // Preserve mode starts from qpdf's source member-to-container map. The
        // linearization plan is built before the resolved ObjStm layout, so an
        // object that is later retained in a source container can still appear
        // in this broad open-document list. qpdf's enqueueObject routes that
        // member through its container and never writes a second plain copy
        // (QPDFWriter.cc:1097-1105); apply the same ownership decision at the
        // final emission boundary. Generate-mode members are not in this list
        // because its planner already separates eligible members from streams.
        if objstm_layout.member_to_container.contains_key(original_ref) {
            continue;
        }
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
        crate::writer::encrypted_strings::write_encryption_dictionary_handle(
            &mut bytes,
            &ctx.encrypt_dict,
        )?; // cov:ignore: LLVM maps this covered encrypted-dictionary continuation to cleanup
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
    let part9_pages: BTreeSet<ObjectRef> = plan
        .optimization
        .as_ref()
        .map(|optimization| optimization.objects_for_root_key(b"Pages"))
        .filter(|pages| !pages.is_empty())
        .unwrap_or_else(|| plan.pages_tree_ref.into_iter().collect());

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
                ContainerPart::Rest
                    if batch.source_container_number.is_some()
                        && batch.members.iter().any(|m| part9_pages.contains(m)) =>
                {
                    // qpdf places the complete /Pages user set before the
                    // remaining lc_other set. If that user set is folded into
                    // a preserved ObjStm, the container inherits the same
                    // head position rather than sorting by its source object
                    // number (QPDF_linearization.cc:1286-1290).
                    (2, 0, 0, 0)
                }
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

/// Resolved state of the destination Catalog's /Extensions /ADBE entry.
struct CatalogAdbeStatus {
    /// Whether an /ADBE key exists under /Extensions in a dictionary.
    /// This mirrors qpdf's key-existence-based removal trigger
    /// (QPDFWriter.cc:1387), not /ExtensionLevel validity.
    has_adbe: bool,
}

/// Prepare the destination Catalog using qpdf's pre-optimization writer setup.
///
/// QPDFWriter::prepareFileForWrite (libqpdf/QPDFWriter.cc:2034-2055)
/// shallow-copies a dictionary-valued indirect /Extensions value onto the
/// Catalog and recursively makes an indirect /ADBE value direct. The
/// linearization plan must observe those replacements before it partitions
/// the graph, so the old indirect objects do not enter the part layout.
///
/// Returns whether the live Catalog was changed. The caller uses that result
/// only to manage flpdf's dirty bookkeeping around this writer-owned
/// preparation; the graph mutation itself remains canonical and live.
fn prepare_linearization_catalog<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<bool> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(false);
    };
    let root = pdf.get_object_handle(root_ref);
    pdf.resolve(&root)?;
    if root.try_as_dictionary()?.is_none() {
        return Ok(false);
    }
    let extensions = root.try_get_key(b"/Extensions")?;
    if extensions.try_as_dictionary()?.is_none() {
        return Ok(false);
    }

    let mut changed = false;
    let extensions = if extensions.is_indirect() {
        let direct = extensions.shallow_copy()?;
        root.replace_key(b"/Extensions", direct.clone())?;
        changed = true;
        direct
    } else {
        extensions
    };

    if extensions.try_has_key(b"/ADBE")? {
        let mut adbe = extensions.try_get_key(b"/ADBE")?;
        if adbe.is_indirect() {
            adbe.make_direct(false)?;
            // `extensions` may still be the exact handle stored in the
            // source Pdf's /Extensions slot (when /Extensions itself was
            // already direct, so the branch above never replaced it):
            // mutating it in place here would corrupt
            // `snapshot_catalog_extensions`'s pre-write capture, since
            // `ObjectHandle` clones share the same underlying cell.
            // Rebuild a fresh top-level dictionary from the current entries
            // (an `ObjectHandle` clone, not a deep copy) instead of
            // `shallow_copy()`: that recursively validates every direct
            // descendant and rejects a direct stream sibling
            // (`libqpdf/QPDF_Stream.cc:140-145`'s "stream objects cannot be
            // cloned"), which would wrongly fail this qpdf-owned
            // preparation step for an unrelated `/Extensions` entry qpdf
            // itself never touches.
            let mut entries = extensions
                .try_as_dictionary()?
                .expect("checked is_some above");
            entries.insert(b"/ADBE".to_vec(), adbe);
            let extensions = ObjectHandle::dictionary(entries.into_iter().collect());
            root.replace_key(b"/Extensions", extensions)?;
            changed = true;
        }
    }

    if changed {
        pdf.mark_object_handle_dirty(&root)?;
    }
    Ok(changed)
}

fn finish_linearization_write<T>(result: Result<T>, restore: Result<()>) -> Result<T> {
    match (result, restore) {
        (Err(error), _) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Resolve Catalog ADBE state from the live handle graph.
fn resolve_catalog_adbe_status<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<CatalogAdbeStatus> {
    const NONE: CatalogAdbeStatus = CatalogAdbeStatus { has_adbe: false };

    // cov:ignore-start: defensive /Root guard. A successful linearization
    // plan always carries the same Catalog root on this Pdf.
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(NONE);
    };
    // cov:ignore-end

    let catalog = pdf.get_object_handle(root_ref);
    pdf.resolve(&catalog)?;
    if catalog.try_as_dictionary()?.is_none() {
        return Ok(NONE);
    }
    let extensions = catalog.try_get_key(b"/Extensions")?;
    if extensions.try_as_dictionary()?.is_none() {
        return Ok(NONE);
    }
    Ok(CatalogAdbeStatus {
        has_adbe: extensions.try_has_key(b"/ADBE")?,
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
/// The production PdfWriter route prepares the Catalog extensions before
/// planning, so indirect /Extensions and /ADBE values are accepted and
/// directized according to qpdf's QPDFWriter::prepareFileForWrite. This
/// lower-level test-only entry point assumes its supplied plan was built
/// after that preparation and does not add a separate extension rejection.
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
    // Capture the caller's raw extension entry before qpdf's permanent
    // pre-plan directization. The output-only ADBE mutation is restored
    // afterward, while the plan observes the prepared live graph.
    let mut catalog_snapshot = crate::writer::snapshot_catalog_extensions(pdf)?;
    let root_ref_before = pdf.root_ref();
    let root_was_dirty_before = root_ref_before.is_some_and(|root_ref| pdf.is_dirty(root_ref));

    let plan_result = (|| {
        let catalog_prepared = prepare_linearization_catalog(pdf)?;
        // The directization is writer-owned preparation, not a caller edit.
        // Clear only its dirty bit when the Catalog started clean; the
        // baseline is refreshed after planning so permanent plan mutations
        // remain dirty through the restore boundary.
        if catalog_prepared && !root_was_dirty_before {
            if let Some(root_ref) = root_ref_before {
                pdf.clear_dirty(root_ref);
            }
        }

        let mode = if crate::writer::force_version_below_1_5(options) {
            crate::writer::ObjectStreamMode::Disable
        } else {
            options.object_streams
        };

        let mut plan_options = options.clone();
        plan_options.object_streams = mode;
        let plan = LinearizationPlan::from_pdf_with_writer_options(pdf, &plan_options)?;
        // qpdf allocates generated ObjStm placeholders before it removes page
        // and Catalog members from the mapping (QPDFWriter.cc:1970-2005,
        // 2141-2161). Count those pre-filter containers for progress even
        // when a later filter leaves one empty and therefore absent from the
        // emitted layout.
        let generated_object_stream_count = if mode == crate::writer::ObjectStreamMode::Generate {
            let compressible = crate::writer::object_streams::compressible_objgens_qpdf_plan(pdf)?;
            crate::writer::object_streams::even_split_into_streams(&compressible.eligible).len()
        } else {
            0
        };
        crate::writer::configure_progress_for_pdf(
            pdf,
            options,
            generated_object_stream_count,
            true,
        )?;
        let renumber = RenumberMap::from_plan(&plan);
        Ok((plan, renumber))
    })();

    // Refresh the dirty baseline exactly once, right after planning/setup
    // above finishes, whether it succeeded or failed partway through (for
    // example a malformed page tree discovered after `LinearizationPlan`
    // has already run `Optimization::prepare_pdf`, which can make a direct
    // `/Outlines` indirect). This is strictly before `write_linearized_impl`
    // below, whose own output-only mutations (such as injecting a fresh
    // `/Extensions /ADBE`) must NOT be folded into the baseline, since
    // `restore_catalog_extensions` exists specifically to undo those.
    crate::writer::record_catalog_snapshot_dirty_baseline(pdf, &mut catalog_snapshot);

    let result = plan_result.and_then(|(plan, renumber)| {
        write_linearized_impl(&plan, &renumber, pdf, options, pass1_path)
    });

    let restore = crate::writer::restore_catalog_extensions(pdf, catalog_snapshot);
    finish_linearization_write(result, restore)
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
        .and_then(|values| values.first().and_then(ObjectHandle::as_string))
        .ok_or_else(|| {
            // cov:ignore-start: unreachable — every branch of finalize_linearized_id
            // constructs a well-formed 2-element string array (see its own body)
            crate::Error::Unsupported(
                "linearization writer: finalize_linearized_id did not return a \
                 well-formed /ID array"
                    .to_string(),
            )
        })?; // cov:ignore-end
    source_trailer_handle.replace_key(b"/ID", finalized_id)?;
    let pass1_source_trailer = source_trailer_handle.shallow_copy()?;
    pass1_source_trailer.replace_key(b"/ID", pass1_id)?;

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
    // `part4_member_set`), but an ineligible outline stream (a stream
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
    let second_half_post_plain: BTreeSet<ObjectRef> =
        if options.object_streams == crate::writer::ObjectStreamMode::Preserve {
            // Preserve containers retain their source object numbers in qpdf's
            // part-7/8/9 sets. Plain objects therefore remain in the same ordered
            // stream as the containers and are placed by their source-number
            // anchors; deferring them would move an ordinary lc_other object past
            // a later source container.
            BTreeSet::new()
        } else {
            plan.part4_rest
                .iter()
                .chain(&plan.part9_outline_objects)
                .copied()
                .filter(|r| {
                    !part4_member_set.contains(r)
                        && !part9_pages.contains(r)
                        && Some(*r) != plan.info_ref
                })
                .collect()
        };
    // First-half mirror of `second_half_post_plain`: under /PageMode /UseOutlines
    // the outline objects route to qpdf part6 (first half) via
    // `part6_outline_objects`. Eligible members ride in a first-half ObjStm batch
    // (open-document or Part-3); an ineligible outline stream (a stream
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
        // qpdf's `lc_first_page_private` sequence emits a plain object that
        // optimization minted for page 0 before the first-half ObjStm
        // container. Keep Part-2 objects in that ordinary first-page sequence.
        // The post-container rule remains for Part-3 shared objects,
        // open-document plain objects, and outlines; their qpdf placement is
        // independently pinned by the corresponding hint/container fixtures.
        first_half_post_plain.extend(
            plan.part3_objects
                .iter()
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
    let open_document_source_container_numbers: Vec<Option<u32>> =
        if options.object_streams == crate::writer::ObjectStreamMode::Preserve {
            let source_container_by_member: BTreeMap<ObjectRef, u32> = pdf
                .source_xref_entries()
                .into_iter()
                .filter_map(|(object_ref, entry)| match entry {
                    crate::XrefEntry::Compressed { stream, .. } => Some((object_ref, stream)),
                    _ => None,
                })
                .collect();
            resolved_batch_plan
                .open_document_batches
                .iter()
                .map(|members| {
                    let source = members
                        .first()
                        .and_then(|member| source_container_by_member.get(member).copied())
                        .ok_or_else(|| {
                            // cov:ignore-start: resolved Preserve batches come directly from source xref compressed-member groups; every non-empty batch therefore has a source container.
                            crate::Error::Unsupported(
                                "linearization writer: preserved open-document ObjStm batch \
                                 has no source container"
                                    .to_string(),
                            )
                        })?; // cov:ignore-end
                    if members.iter().any(|member| {
                        source_container_by_member.get(member).copied() != Some(source)
                        // cov:ignore: objstm_batches_preserve groups each non-empty batch by one source container
                    }) {
                        // cov:ignore-start: source-container homogeneity is guaranteed by objstm_batches_preserve's per-container grouping.
                        return Err(crate::Error::Unsupported(
                            "linearization writer: preserved open-document ObjStm batch \
                             combines multiple source containers"
                                .to_string(),
                        ));
                        // cov:ignore-end
                    }
                    Ok(Some(source))
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            vec![None; resolved_batch_plan.open_document_batches.len()]
        };
    let relocation = if emits_object_streams {
        local_renumber.place_objstm_members_per_half(
            &resolved_batch_plan.open_document_batches,
            &open_document_source_container_numbers,
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
                crate::writer::resolve_metadata_stream_ref(pdf)?
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
                .try_get_key(b"/EncryptMetadata")?
                .as_boolean()
                .unwrap_or(true);
            let metadata_ref = if encrypt_metadata {
                None
            } else {
                crate::writer::resolve_metadata_stream_ref(pdf)?
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
    let source_ext = pdf.adobe_extension_level()?.unwrap_or(0);
    let (eff_version, eff_ext) =
        effective_pdf_version_and_ext(&source_ver, source_ext, options, true, emits_object_streams);
    let adbe_status = resolve_catalog_adbe_status(pdf)?;
    if eff_ext > 0 || adbe_status.has_adbe {
        if eff_ext > 0 {
            inject_adbe_extension(pdf, eff_version, eff_ext)?;
        } else {
            strip_adbe_extension(pdf, eff_version, eff_ext)?;
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
                let source_container_number =
                    preserved_source_container_number(container, &source_container_by_member)?;
                keys.insert(container.container_new_num, (0, source_container_number));
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
    let id_writer: Option<crate::pdf_syntax::TrailerIdWriter> = match &classic_det_id {
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

    let mut old_to_new: BTreeMap<ObjectRef, ObjectRef> = renumber
        .iter_in_layout_order()
        .map(|(new_ref, old_ref)| (old_ref, new_ref))
        .collect();

    // qpdf's getRenumberedObjGen reports the source ObjStm container's logical
    // output identity even though the output container is a freshly serialized
    // object. The source container is intentionally absent from the ordinary
    // linearization plan (only its members are body objects), so add this
    // canonical writer-result mapping from the resolved Preserve layout. This
    // lets test_renumber compare the same source object universe as qpdf
    // without making the helper reconstruct container ownership.
    if options.object_streams == crate::writer::ObjectStreamMode::Preserve
        && !objstm_layout.is_empty()
    {
        let source_container_by_member: BTreeMap<ObjectRef, u32> = pdf
            .source_xref_entries()
            .into_iter()
            .filter_map(|(object_ref, entry)| match entry {
                crate::XrefEntry::Compressed { stream, .. } => Some((object_ref, stream)),
                _ => None,
            })
            .collect();
        for container in objstm_layout
            .open_document
            .iter()
            .chain(&objstm_layout.part3)
            .chain(&objstm_layout.part4)
        {
            let source_container_number =
                preserved_source_container_number(container, &source_container_by_member)?;
            old_to_new.insert(
                ObjectRef::new(source_container_number, 0),
                ObjectRef::new(container.container_new_num, 0),
            );
        }
    }

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
