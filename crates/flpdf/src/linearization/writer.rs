//! Layout writer — orchestrates the full linearized PDF output (sub-task 2.8).
//!
//! This module assembles the six-part Annex F layout in correct order, tracks
//! byte offsets for back-patching, and returns the finished bytes together with
//! all offset information that the back-patcher (sub-task 2.9) needs.
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
//! Back-patching the placeholder values is the responsibility of sub-task 2.9.
//! This module returns `LinearizedOffsets` containing all information required
//! for that step.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use crate::linearization::hint_page::{bits_needed, PageOffsetHintTable};
use crate::linearization::hint_shared::SharedObjectHintTable;
use crate::linearization::hint_stream::encode_hint_stream;
use crate::linearization::part1::{Part1Bytes, Part1Placeholders};
use crate::linearization::plan::LinearizationPlan;
use crate::linearization::renumber::RenumberMap;
use crate::{Dictionary, Object, ObjectRef, Pdf, Result, Stream};

// ---------------------------------------------------------------------------
// Public result types
// ---------------------------------------------------------------------------

/// Byte offsets and derived values returned by [`write_linearized`].
///
/// All values are absolute byte positions within `LinearizedDocument::bytes`
/// unless stated otherwise.  The back-patcher (sub-task 2.9) uses these to
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

    /// Placeholder byte ranges inside the Part 1 bytes, forwarded to
    /// sub-task 2.9 for in-place patching.
    pub part1_placeholders: Part1Placeholders,

    /// `new_object_number → byte_offset` map covering every object in the
    /// linearized file.  Used by sub-task 2.11 for structural verification.
    pub xref_offsets: BTreeMap<u32, usize>,
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
// Internal helpers
// ---------------------------------------------------------------------------

/// Deep-clone `object`, replacing every `Reference(r)` with the renumbered
/// equivalent from `renumber`.  Returns an error if a reference cannot be
/// mapped — leaving an un-renumbered reference in a renumbered file would
/// produce a mixed old/new object number that the generated xref does not
/// describe, silently corrupting the linearized output.
///
/// Stream data bytes are **not** inspected — they are opaque binary blobs.
fn renumber_object(object: &Object, renumber: &RenumberMap) -> Result<Object> {
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
                renumbered.push(renumber_object(e, renumber)?);
            }
            Ok(Object::Array(renumbered))
        }
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in dict.iter() {
                new_dict.insert(key, renumber_object(value, renumber)?);
            }
            Ok(Object::Dictionary(new_dict))
        }
        Object::Stream(stream) => {
            // Renumber the dictionary; leave the stream data bytes alone.
            let mut new_dict = Dictionary::new();
            for (key, value) in stream.dict.iter() {
                new_dict.insert(key, renumber_object(value, renumber)?);
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

/// Write a Part 1 xref subsection (the linearization parameter dict only) plus
/// a minimal trailer, then return the `startxref` offset of this xref block.
///
/// The Part 1 xref is required by the linearized PDF spec so that a viewer can
/// quickly locate the linearization parameter dict without parsing the whole
/// file.  It covers only the param-dict object (at whatever number the
/// renumber map assigned it); all other objects are recorded in Part 6.
fn write_part1_xref_and_trailer(
    bytes: &mut Vec<u8>,
    param_dict_offset: usize,
    param_dict_obj_number: u32,
    total_object_count: u32,
    catalog_new_ref: ObjectRef,
) -> usize {
    let xref_offset = bytes.len();

    // Subsection: param dict object only.
    bytes.extend_from_slice(format!("xref\n{param_dict_obj_number} 1\n").as_bytes());
    bytes.extend_from_slice(format!("{:010} 00000 n \n", param_dict_offset).as_bytes());

    // Minimal trailer for Part 1.
    let mut trailer = Dictionary::new();
    trailer.insert("Size", Object::Integer(i64::from(total_object_count)));
    trailer.insert("Root", Object::Reference(catalog_new_ref));
    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(bytes);
    // startxref points to the xref keyword offset of this Part 1 xref section.
    // PDF spec §7.5.5 requires startxref to be the byte offset of the xref keyword.
    bytes.extend_from_slice(format!("\nstartxref\n{}\n%%EOF\n", xref_offset).as_bytes());

    xref_offset
}

/// Write the Part 6 cross-reference table covering all objects (0 through N),
/// followed by the main trailer.
///
/// Returns `(xref_keyword_offset, xref_first_entry_offset)` where:
/// - `xref_keyword_offset` is the byte offset of the `xref` keyword
/// - `xref_first_entry_offset` is the byte offset of the first xref entry
///   (after the `xref\n0 N\n` header), which is the correct `/T` value per
///   qpdf's linearization checker.
fn write_main_xref_and_trailer(
    bytes: &mut Vec<u8>,
    xref_offsets: &BTreeMap<u32, usize>,
    total_count: u32, // /Size — highest object number + 1
    catalog_new_ref: ObjectRef,
    info_new_ref: Option<ObjectRef>,
) -> (usize, usize) {
    let xref_start = bytes.len();

    // Dense table: objects 0 .. total_count.
    let xref_header = format!("xref\n0 {}\n", total_count);
    bytes.extend_from_slice(xref_header.as_bytes());
    let xref_first_entry_offset = bytes.len();
    // Object 0 — free head.
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..total_count {
        match xref_offsets.get(&number) {
            Some(offset) => {
                bytes.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes())
            }
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }

    // Main trailer.
    let mut trailer = Dictionary::new();
    trailer.insert("Size", Object::Integer(i64::from(total_count)));
    trailer.insert("Root", Object::Reference(catalog_new_ref));
    if let Some(info_ref) = info_new_ref {
        trailer.insert("Info", Object::Reference(info_ref));
    }
    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(bytes);
    bytes.extend_from_slice(format!("\nstartxref\n{}\n%%EOF\n", xref_start).as_bytes());

    (xref_start, xref_first_entry_offset)
}

/// Build the hint stream object bytes for a given compressed payload.
///
/// Returns the full object bytes (header + dict + stream + endobj) and the
/// byte length of the object for `/H[1]`.
fn build_hint_stream_object(
    new_ref: ObjectRef,
    compressed_payload: &[u8],
    shared_section_offset: usize,
) -> Object {
    let compressed_len = compressed_payload.len();
    let mut hint_dict = Dictionary::new();
    hint_dict.insert("Length", Object::Integer(compressed_len as i64));
    hint_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    hint_dict.insert("S", Object::Integer(shared_section_offset as i64));
    let _ = new_ref; // ref is used at call site via append_object
    Object::Stream(Stream::new(hint_dict, compressed_payload.to_vec()))
}

/// Perform a complete single-pass write of the linearized PDF body.
///
/// Returns `(bytes, xref_offsets, hint_stream_offset, hint_stream_obj_total_len,
///           end_of_first_page_offset, last_xref_offset, last_xref_first_entry_offset)`.
///
/// `hint_compressed` is the compressed payload to use for the hint stream object.
/// `hint_shared_section_offset` is the `/S` value (offset within the uncompressed stream).
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
) -> Result<(
    Vec<u8>,
    BTreeMap<u32, usize>,
    usize, // hint_stream_offset
    usize, // hint_stream_obj_total_len
    usize, // end_of_first_page_offset
    usize, // last_xref_offset (xref keyword position)
    usize, // last_xref_first_entry_offset (= /T value per qpdf's convention)
)> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut xref_offsets: BTreeMap<u32, usize> = BTreeMap::new();

    // Part 1
    let param_dict_obj_number = renumber.param_dict_ref().number;
    let param_dict_absolute_offset = part1.obj1_offset;
    bytes.extend_from_slice(&part1.bytes);
    xref_offsets.insert(param_dict_obj_number, param_dict_absolute_offset);
    write_part1_xref_and_trailer(
        &mut bytes,
        param_dict_absolute_offset,
        param_dict_obj_number,
        total_count,
        catalog_new_ref,
    );

    // Hint stream object
    let hint_new_ref = ObjectRef::new(hint_stream_new_num, 0);
    let hint_obj =
        build_hint_stream_object(hint_new_ref, hint_compressed, hint_shared_section_offset);
    let hint_stream_offset = append_object(&mut bytes, hint_new_ref, &hint_obj);
    xref_offsets.insert(hint_stream_new_num, hint_stream_offset);
    let hint_stream_obj_total_len = bytes.len() - hint_stream_offset;

    // Part 3 (Annex F): first-page body — Plan.part2_objects (page-0 private objects)
    for original_ref in &plan.part2_objects {
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part2 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve(*original_ref)?;
        let renumbered = renumber_object(&object, renumber)?;
        let offset = append_object(&mut bytes, new_ref, &renumbered);
        xref_offsets.insert(new_ref.number, offset);
    }

    // Part 3 (Annex F) continued: shared objects sit INSIDE the first-page
    // section.  qpdf's hint table validator counts page 0's object_count
    // as Part-2 + Part-3 (all objects before /E), and /E itself is the
    // byte after the last shared object.  Putting shared objects after /E
    // causes "/E mismatch" and "object count for page 0 = N; computed = M"
    // warnings.
    for original_ref in &plan.part3_objects {
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part3 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve(*original_ref)?;
        let renumbered = renumber_object(&object, renumber)?;
        let offset = append_object(&mut bytes, new_ref, &renumbered);
        xref_offsets.insert(new_ref.number, offset);
    }

    // /E: end of first-page section, AFTER both Part-2 and Part-3.
    let end_of_first_page_offset = bytes.len();

    // Part 5 (Annex F): remaining body — derived view of all Part-4
    // sub-partitions in writer-emission order.
    for original_ref in &plan.part4_objects() {
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part4 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve(*original_ref)?;
        let renumbered = renumber_object(&object, renumber)?;
        let offset = append_object(&mut bytes, new_ref, &renumbered);
        xref_offsets.insert(new_ref.number, offset);
    }

    // Part 6: main xref + trailer
    let (last_xref_offset, last_xref_first_entry_offset) = write_main_xref_and_trailer(
        &mut bytes,
        &xref_offsets,
        total_count,
        catalog_new_ref,
        info_new_ref,
    );

    Ok((
        bytes,
        xref_offsets,
        hint_stream_offset,
        hint_stream_obj_total_len,
        end_of_first_page_offset,
        last_xref_offset,
        last_xref_first_entry_offset,
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
/// [`LinearizedOffsets`] needed for back-patching (sub-task 2.9).
pub fn write_linearized<R: Read + Seek>(
    plan: &LinearizationPlan,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
) -> Result<LinearizedDocument> {
    // ------------------------------------------------------------------
    // Pre-compute values that do not change across iterations.
    // ------------------------------------------------------------------
    let part1 = Part1Bytes::build(plan, renumber);
    let part1_placeholders = part1.placeholders.clone();

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
    // Highest object number actually used in the output: the largest slot in
    // the renumber map (`len()` already returns that). Adding 1 yields the
    // /Size value (count = highest_number + 1, because object numbering is
    // 1-based and Size counts the unused free-list entry at 0).
    let total_count: u32 = renumber.len() as u32 + 1;

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

    for iter in 0..max_iters {
        let (
            bytes,
            xref_offsets,
            hint_stream_offset,
            hint_stream_obj_total_len,
            end_of_first_page_offset,
            last_xref_offset,
            last_xref_first_entry_offset,
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
        let part3_byte_len: u64 = plan
            .part3_objects
            .iter()
            .map(|orig| {
                renumber
                    .new_for_original(*orig)
                    .and_then(|new_ref| byte_lengths.get(&new_ref.number).copied())
                    .unwrap_or(0) as u64
            })
            .sum();

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
        if !per_page_byte_lengths.is_empty() {
            let least_pl = per_page_byte_lengths.iter().copied().min().unwrap_or(0);
            let max_pl = per_page_byte_lengths.iter().copied().max().unwrap_or(0);
            po_table.header.least_page_length = least_pl;
            po_table.header.bits_page_length_delta = bits_needed(max_pl.saturating_sub(least_pl));
            for (i, &bl) in per_page_byte_lengths.iter().enumerate() {
                if i < po_table.entries.len() {
                    po_table.entries[i].page_length_minus_least = bl.saturating_sub(least_pl);
                }
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

            // Location of first shared section object (= shared_hints[0] = part2[0] = page dict).
            //
            // The probe-pass xref_offsets already account for the hint stream
            // object bytes (it is written inline in do_write_pass before Part 2/3
            // objects).  Do NOT apply adjusted_offset here — that would add
            // hint_stream_obj_total_len a second time and produce an offset
            // that is too large by exactly the hint stream size.
            let first_shared_orig = plan.shared_hints[0].object_ref;
            let first_shared_new_num = renumber
                .new_for_original(first_shared_orig)
                .ok_or_else(|| {
                    crate::Error::Unsupported(format!(
                        "first shared hint object {} has no renumber entry",
                        first_shared_orig
                    ))
                })?
                .number;
            let first_shared_off = xref_offsets
                .get(&first_shared_new_num)
                .copied()
                .ok_or_else(|| {
                    crate::Error::Unsupported(format!(
                        "first shared hint object (new #{}) has no probed offset",
                        first_shared_new_num
                    ))
                })?;
            so_table.header.location = first_shared_off as u64;

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
            )?;
            final_bytes = bytes_final;
            final_xref_offsets = xref_offsets_final;
            final_hint_stream_offset = hint_off_final;
            final_hint_stream_obj_total_len = hint_len_final;
            final_end_of_first_page_offset = efp_final;
            final_last_xref_keyword_offset = lxr_final;
            final_last_xref_first_entry_offset = lxr_first_final;
            break;
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
        write_linearized(&plan, &renumber, &mut pdf2).expect("write_linearized")
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
        let part1_len = Part1Bytes::build(&plan, &renumber).byte_length();

        let mut pdf2 = open_tiny_pdf();
        let doc = write_linearized(&plan, &renumber, &mut pdf2).expect("write");

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
    // 14. HIGH fix: Part 1 startxref value equals the offset of the first
    //     `xref` keyword in the file (not obj1's offset).
    //
    //     PDF §7.5.5: startxref shall give the byte offset of the xref keyword.
    //     Before the fix, the value was obj1_offset (= 15), which pointed to
    //     `1 0 obj`, causing parsers to fail finding the xref table.
    // -----------------------------------------------------------------------
    #[test]
    fn part1_startxref_points_to_xref_keyword() {
        let doc = build_linearized();
        let bytes = &doc.bytes;

        // Find the byte offset of the first `xref` keyword in the file.
        let first_xref_offset = bytes
            .windows(4)
            .position(|w| w == b"xref")
            .expect("linearized output must contain at least one xref keyword");

        // Locate `startxref\n` in Part 1 (before any `xref` that is *not* the
        // Part 1 xref, i.e. search only up to the Part 6 xref).
        // We scan for b"startxref\n" and take the first occurrence.
        let startxref_needle = b"startxref\n";
        let startxref_pos = bytes
            .windows(startxref_needle.len())
            .position(|w| w == startxref_needle)
            .expect("linearized output must contain startxref");

        // Read the decimal number immediately after "startxref\n".
        let value_start = startxref_pos + startxref_needle.len();
        let value_end = bytes[value_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| value_start + p)
            .expect("startxref value must be terminated by newline");
        let value_str =
            std::str::from_utf8(&bytes[value_start..value_end]).expect("startxref value is UTF-8");
        let part1_startxref_value: usize = value_str
            .trim()
            .parse()
            .expect("startxref value must be a decimal integer");

        assert_eq!(
            part1_startxref_value, first_xref_offset,
            "Part 1 startxref ({part1_startxref_value}) must equal the offset of the \
             first xref keyword ({first_xref_offset}), not the offset of `1 0 obj`"
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
        let doc = write_linearized(&plan, &renumber, &mut pdf2).expect("write_linearized");

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
}
