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
//! Part 1       | header + linearization param dict (object 1) with placeholders
//!              | + Part 1 xref subsection (object 1 only) + trailer
//! Part 2       | hint stream object (compressed, with /Filter /FlateDecode /S …)
//! Part 3       | first-page body — Plan.part2_objects with renumbered refs
//! Part 4       | shared/catalog/info — Plan.part3_objects with renumbered refs
//! Part 5       | remaining body — Plan.part4_objects with renumbered refs
//! Part 6       | cross-reference table for all objects + trailer
//! ```
//!
//! **Terminology note**: the `LinearizationPlan` field names (`part2_objects`,
//! `part3_objects`, `part4_objects`) do **not** correspond to the Annex F "Part"
//! numbers above.  Mapping:
//!
//! - `Plan.part2_objects` → Annex F Part 3 (first-page body)
//! - `Plan.part3_objects` → Annex F Part 4 (shared/catalog/info)
//! - `Plan.part4_objects` → Annex F Part 5 (remaining body)
//!
//! The hint stream (Annex F Part 2) does **not** appear in the plan's object
//! lists; its new object number is `renumber.len() + 1`.
//!
//! # Scope
//!
//! Back-patching the placeholder values is the responsibility of sub-task 2.9.
//! This module returns `LinearizedOffsets` containing all information required
//! for that step.

use std::collections::BTreeMap;
use std::io::{Read, Seek};

use crate::linearization::hint_page::PageOffsetHintTable;
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

    /// Byte offset of the Part 6 cross-reference table (`xref` keyword) —
    /// corresponds to `/T`.
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
/// equivalent from `renumber`.  References whose original number is not in the
/// map are left unchanged (with a debug assertion so tests catch accidents).
///
/// Stream data bytes are **not** inspected — they are opaque binary blobs.
fn renumber_object(object: &Object, renumber: &RenumberMap) -> Object {
    match object {
        Object::Reference(r) => {
            if let Some(new_ref) = renumber.new_for_original(*r) {
                Object::Reference(new_ref)
            } else {
                // Reference not in the map — leave as-is but flag in debug.
                debug_assert!(
                    false,
                    "renumber_object: no mapping for {r} — emitting original reference"
                );
                Object::Reference(*r)
            }
        }
        Object::Array(elements) => Object::Array(
            elements
                .iter()
                .map(|e| renumber_object(e, renumber))
                .collect(),
        ),
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (key, value) in dict.iter() {
                new_dict.insert(key, renumber_object(value, renumber));
            }
            Object::Dictionary(new_dict)
        }
        Object::Stream(stream) => {
            // Renumber the dictionary; leave the stream data bytes alone.
            let mut new_dict = Dictionary::new();
            for (key, value) in stream.dict.iter() {
                new_dict.insert(key, renumber_object(value, renumber));
            }
            Object::Stream(Stream::new(new_dict, stream.data.clone()))
        }
        // Scalar types contain no references — clone unchanged.
        _ => object.clone(),
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

/// Write a Part 1 xref subsection (object 1 only) plus a minimal trailer, then
/// return the `startxref` offset of this xref block.
///
/// The Part 1 xref is required by the linearized PDF spec so that a viewer can
/// quickly locate the linearization parameter dict without parsing the whole
/// file.  It covers only object 1; all other objects are recorded in Part 6.
fn write_part1_xref_and_trailer(
    bytes: &mut Vec<u8>,
    obj1_offset: usize,
    total_object_count: u32,
    catalog_new_ref: ObjectRef,
) -> usize {
    let xref_offset = bytes.len();

    // Subsection: object 1 only.
    bytes.extend_from_slice(b"xref\n1 1\n");
    bytes.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());

    // Minimal trailer for Part 1.
    let mut trailer = Dictionary::new();
    trailer.insert("Size", Object::Integer(i64::from(total_object_count)));
    trailer.insert("Root", Object::Reference(catalog_new_ref));
    bytes.extend_from_slice(b"trailer\n");
    trailer.write_pdf(bytes);
    // startxref points to object 1's absolute offset (not this xref's offset)
    // per Annex F.  Readers use it to locate the param dict.
    bytes.extend_from_slice(format!("\nstartxref\n{}\n%%EOF\n", obj1_offset).as_bytes());

    xref_offset
}

/// Write the Part 6 cross-reference table covering all objects (0 through N),
/// followed by the main trailer.
///
/// Returns the byte offset of the `xref` keyword (= `/T` value in param dict).
fn write_main_xref_and_trailer(
    bytes: &mut Vec<u8>,
    xref_offsets: &BTreeMap<u32, usize>,
    total_count: u32, // /Size — highest object number + 1
    catalog_new_ref: ObjectRef,
    info_new_ref: Option<ObjectRef>,
) -> usize {
    let xref_start = bytes.len();

    // Dense table: objects 0 .. total_count.
    bytes.extend_from_slice(format!("xref\n0 {}\n", total_count).as_bytes());
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

    xref_start
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
/// 1. Emits Part 1: header + linearization param dict (object 1) with
///    placeholder numeric values, followed by a one-object xref subsection
///    and trailer.
/// 2. Emits the hint stream object (Annex F Part 2).
/// 3. Emits the first-page body objects (`Plan.part2_objects` — Annex F Part 3).
/// 4. Emits the shared/catalog/info objects (`Plan.part3_objects` — Annex F Part 4).
/// 5. Emits the remaining body objects (`Plan.part4_objects` — Annex F Part 5).
/// 6. Emits the main cross-reference table and trailer (Annex F Part 6).
///
/// Returns [`LinearizedDocument`] containing both the bytes and the
/// [`LinearizedOffsets`] needed for back-patching (sub-task 2.9).
pub fn write_linearized<R: Read + Seek>(
    plan: &LinearizationPlan,
    renumber: &RenumberMap,
    pdf: &mut Pdf<R>,
) -> Result<LinearizedDocument> {
    let mut bytes: Vec<u8> = Vec::new();
    // new_number → absolute byte offset
    let mut xref_offsets: BTreeMap<u32, usize> = BTreeMap::new();

    // ------------------------------------------------------------------
    // Part 1: header + linearization param dict + xref subsection
    // ------------------------------------------------------------------
    // Object 1 starts at offset 0 in the Part 1 bytes, but Part 1 bytes
    // includes the file header before object 1.  We need the absolute
    // offset of the `1 0 obj` token within `bytes`.
    let part1 = Part1Bytes::build(plan, renumber);
    let part1_placeholders = part1.placeholders.clone();

    // The `1 0 obj` token starts after the two header lines:
    //   "%PDF-1.7\n"      (9 bytes)
    //   "%<4 binary bytes>\n"   (6 bytes)
    // = 15 bytes.
    let obj1_absolute_offset = 15; // fixed by Part1Bytes::build format
    bytes.extend_from_slice(&part1.bytes);
    xref_offsets.insert(1, obj1_absolute_offset);

    // Determine the catalog's new object number (for the trailer's /Root).
    let catalog_new_ref: ObjectRef = plan
        .root_ref
        .and_then(|orig| renumber.new_for_original(orig))
        .unwrap_or_else(|| ObjectRef::new(2, 0)); // fallback — should always resolve

    // Write Part 1 xref subsection (object 1 only) + minimal trailer.
    // `total_count_for_part1` is the full object count so viewers can validate.
    // Hint stream gets number `renumber.len() + 1`; Size must cover it.
    let hint_stream_new_num: u32 = renumber.len() as u32 + 1;
    let total_count: u32 = hint_stream_new_num + 1; // 0 .. hint_stream_new_num inclusive
    write_part1_xref_and_trailer(
        &mut bytes,
        obj1_absolute_offset,
        total_count,
        catalog_new_ref,
    );

    // ------------------------------------------------------------------
    // Part 2 (Annex F): hint stream object
    // ------------------------------------------------------------------
    let page_offset_table = PageOffsetHintTable::from_plan(plan, renumber);
    let shared_object_table = SharedObjectHintTable::from_plan(plan, renumber);
    let hint_bytes = encode_hint_stream(&page_offset_table, &shared_object_table);

    let hint_stream_compressed = &hint_bytes.compressed;
    let shared_section_s = hint_bytes.shared_section_offset_in_uncompressed;

    // Build the hint stream object dictionary.
    let compressed_len = hint_stream_compressed.len();
    let mut hint_dict = Dictionary::new();
    hint_dict.insert(
        "Length",
        Object::Integer(i64::try_from(compressed_len).map_err(|_| {
            crate::Error::Unsupported("hint stream /Length does not fit i64".to_string())
        })?),
    );
    hint_dict.insert("Filter", Object::Name(b"FlateDecode".to_vec()));
    hint_dict.insert(
        "S",
        Object::Integer(i64::try_from(shared_section_s).map_err(|_| {
            crate::Error::Unsupported("hint stream /S does not fit i64".to_string())
        })?),
    );
    let hint_stream_object = Object::Stream(Stream::new(hint_dict, hint_stream_compressed.clone()));

    let hint_new_ref = ObjectRef::new(hint_stream_new_num, 0);
    let hint_stream_offset = append_object(&mut bytes, hint_new_ref, &hint_stream_object);
    xref_offsets.insert(hint_stream_new_num, hint_stream_offset);

    // ------------------------------------------------------------------
    // Part 3 (Annex F): first-page body — Plan.part2_objects
    // ------------------------------------------------------------------
    // The first-page object (/O) is the first Part-2 object (new number 2).
    let first_page_object_new_num: u32 = 2; // always the first Part-2 object

    for original_ref in &plan.part2_objects {
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part2 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve(*original_ref)?;
        let renumbered = renumber_object(&object, renumber);
        let offset = append_object(&mut bytes, new_ref, &renumbered);
        xref_offsets.insert(new_ref.number, offset);
    }

    // /E — offset immediately after the last first-page body byte.
    let end_of_first_page_offset = bytes.len();

    // ------------------------------------------------------------------
    // Part 4 (Annex F): shared/catalog/info — Plan.part3_objects
    // ------------------------------------------------------------------
    for original_ref in &plan.part3_objects {
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part3 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve(*original_ref)?;
        let renumbered = renumber_object(&object, renumber);
        let offset = append_object(&mut bytes, new_ref, &renumbered);
        xref_offsets.insert(new_ref.number, offset);
    }

    // ------------------------------------------------------------------
    // Part 5 (Annex F): remaining body — Plan.part4_objects
    // ------------------------------------------------------------------
    for original_ref in &plan.part4_objects {
        let Some(new_ref) = renumber.new_for_original(*original_ref) else {
            return Err(crate::Error::Unsupported(format!(
                "part4 object {} has no renumber entry",
                original_ref
            )));
        };
        let object = pdf.resolve(*original_ref)?;
        let renumbered = renumber_object(&object, renumber);
        let offset = append_object(&mut bytes, new_ref, &renumbered);
        xref_offsets.insert(new_ref.number, offset);
    }

    // ------------------------------------------------------------------
    // Part 6 (Annex F): main cross-reference table + trailer
    // ------------------------------------------------------------------
    // Determine /Info ref if the original PDF had one.
    let info_new_ref: Option<ObjectRef> = pdf
        .trailer()
        .get_ref("Info")
        .and_then(|orig| renumber.new_for_original(orig));

    let last_xref_offset = write_main_xref_and_trailer(
        &mut bytes,
        &xref_offsets,
        total_count,
        catalog_new_ref,
        info_new_ref,
    );

    // ------------------------------------------------------------------
    // Assemble offsets
    // ------------------------------------------------------------------
    let file_length = bytes.len();
    let page_count = plan.page_hints.len() as u32;

    let offsets = LinearizedOffsets {
        file_length,
        hint_stream_offset,
        hint_stream_length: compressed_len,
        first_page_object_new_num,
        end_of_first_page_offset,
        last_xref_offset,
        page_count,
        part1_placeholders,
        xref_offsets,
    };

    Ok(LinearizedDocument { bytes, offsets })
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
    // 10. xref_offsets[1] equals the obj1 absolute offset (15)
    // -----------------------------------------------------------------------
    #[test]
    fn xref_offsets_obj1_is_correct() {
        let doc = build_linearized();
        let obj1_off = doc
            .offsets
            .xref_offsets
            .get(&1)
            .copied()
            .unwrap_or(usize::MAX);
        assert_eq!(
            obj1_off, 15,
            "object 1 must start at byte 15 (after two header lines)"
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
}
