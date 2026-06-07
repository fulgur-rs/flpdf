//! Back-patcher for Part 1 linearization parameter dictionary.
//!
//! After layout is complete and all byte offsets are known, this module
//! rewrites the linearization parameter dictionary with **variable-width**
//! decimal values (qpdf byte format):
//!
//! | Param dict key | Source field in [`LinearizedOffsets`]       |
//! |----------------|---------------------------------------------|
//! | `/L`           | `file_length`                               |
//! | `/H[0]`        | `hint_stream_offset`                        |
//! | `/H[1]`        | `hint_stream_length`                        |
//! | `/O`           | `first_page_object_new_num`                 |
//! | `/E`           | `end_of_first_page_offset`                  |
//! | `/T`           | `last_xref_offset`                          |
//! | `/N`           | `page_count`                                |
//!
//! # Splice-and-pad discipline
//!
//! [`Part1Bytes::build`](crate::linearization::part1::Part1Bytes::build) lays
//! the dict out with 10-digit zero-padded placeholders followed by a fixed
//! [`PARAM_DICT_TRAILING_PAD`](crate::linearization::part1::PARAM_DICT_TRAILING_PAD)
//! byte reserve of ASCII space.  This module rewrites the entire
//! `dict_writable_region` in a single splice:
//!
//! ```text
//! "<< /Linearized 1 /L V /H [ V V ] /O V /E V /N V /T V >>\nendobj\n"
//! + ' ' × (region_len − new_dict_len)
//! ```
//!
//! The compaction is monotone (each value emits as ≤ 10 decimal bytes) so the
//! reserve always covers it, and the total Part 1 byte length stays constant.
//! Downstream offsets — which the writer computed against the placeholder
//! layout — therefore never shift.
//!
//! `LinearizedOffsets.part1_placeholders` is updated post-splice to point at
//! the new variable-width value bytes, so callers can keep using it to inspect
//! the back-patched values.
//!
//! # Errors
//!
//! Returns [`crate::Error::Unsupported`] when:
//! * any numeric value exceeds 10^10 − 1 and therefore cannot fit even at the
//!   upper-bound width,
//! * the rendered dict body does not fit within `dict_writable_region` (would
//!   only happen on an internal layout invariant violation), or
//! * the `/Prev` placeholder in the first trailer is malformed.
//!
//! All checks run as a preflight pass before any byte is mutated, so an `Err`
//! leaves `bytes` byte-wise unchanged.
//!
//! # Usage
//!
//! ```rust,ignore
//! let mut doc = write_linearized(&plan, &renumber, &mut pdf)?;
//! doc.back_patch()?;
//! // doc.bytes now contains the complete, patched linearized PDF.
//! ```

use std::ops::Range;

use crate::linearization::part1::{Part1Placeholders, PLACEHOLDER_WIDTH};
use crate::linearization::writer::{LinearizedDocument, LinearizedOffsets, PREV_PLACEHOLDER_WIDTH};
use crate::Result;

// ---------------------------------------------------------------------------
// Internal constant
// ---------------------------------------------------------------------------

/// Maximum value that fits in [`PLACEHOLDER_WIDTH`] decimal digits.
/// = 10^10 − 1 = 9_999_999_999
const MAX_PLACEHOLDER_VALUE: u64 = {
    // 10^PLACEHOLDER_WIDTH - 1
    // PLACEHOLDER_WIDTH is 10, so 10_000_000_000 - 1
    10_000_000_000 - 1
};

// ---------------------------------------------------------------------------
// Variable-width compaction helpers
// ---------------------------------------------------------------------------

/// The seven numeric values that go into the linearization parameter dict, in
/// the same emission order as the placeholders.
#[derive(Debug, Clone, Copy)]
struct Part1Values {
    l: u64,
    h_offset: u64,
    h_length: u64,
    o: u64,
    e: u64,
    n: u64,
    t: u64,
}

impl Part1Values {
    fn from_offsets(offsets: &LinearizedOffsets) -> Self {
        Self {
            l: offsets.file_length as u64,
            h_offset: offsets.hint_stream_offset as u64,
            h_length: offsets.hint_stream_length as u64,
            o: u64::from(offsets.first_page_object_new_num),
            e: offsets.end_of_first_page_offset as u64,
            n: u64::from(offsets.page_count),
            t: offsets.last_xref_offset as u64,
        }
    }
}

/// Render the variable-width dict body (including the trailing `\nendobj\n`
/// marker).  Layout must match `Part1Bytes::build`'s placeholder layout
/// exactly: qpdf-aligned key order
/// `/Linearized /L /H /O /E /N /T`.
fn render_compact_dict(values: &Part1Values) -> Vec<u8> {
    format!(
        "<< /Linearized 1 /L {} /H [ {} {} ] /O {} /E {} /N {} /T {} >>\nendobj\n",
        values.l, values.h_offset, values.h_length, values.o, values.e, values.n, values.t,
    )
    .into_bytes()
}

/// Compute the post-splice byte range for each value field within the
/// rewritten dict region.
///
/// Mirrors [`render_compact_dict`]'s template — any change to that template
/// must be reflected here so `LinearizedOffsets.part1_placeholders` keeps
/// pointing at the rewritten value bytes.
fn compute_post_splice_placeholders(
    region_start: usize,
    values: &Part1Values,
) -> Part1Placeholders {
    // Literal prefixes in `render_compact_dict`, in field emission order.
    const PREFIXES: [&[u8]; 7] = [
        b"<< /Linearized 1 /L ", // before /L
        b" /H [ ",               // before /H[0]
        b" ",                    // before /H[1]
        b" ] /O ",               // before /O
        b" /E ",                 // before /E
        b" /N ",                 // before /N
        b" /T ",                 // before /T
    ];
    let widths = [
        digit_width(values.l),
        digit_width(values.h_offset),
        digit_width(values.h_length),
        digit_width(values.o),
        digit_width(values.e),
        digit_width(values.n),
        digit_width(values.t),
    ];

    let mut cursor = region_start;
    let mut ranges: [Range<usize>; 7] = std::array::from_fn(|_| 0..0);
    for i in 0..7 {
        cursor += PREFIXES[i].len();
        ranges[i] = cursor..cursor + widths[i];
        cursor += widths[i];
    }
    Part1Placeholders {
        l: ranges[0].clone(),
        h_offset: ranges[1].clone(),
        h_length: ranges[2].clone(),
        o: ranges[3].clone(),
        e: ranges[4].clone(),
        n: ranges[5].clone(),
        t: ranges[6].clone(),
    }
}

/// Decimal digit count for `n` (always ≥ 1: `digit_width(0) == 1`).
fn digit_width(n: u64) -> usize {
    // `checked_ilog10()` is None for 0 and ilog10(x) + 1 for any positive x;
    // it compiles down to a leading-zero-count + LUT, no loop or division.
    (n.checked_ilog10().unwrap_or(0) + 1) as usize
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Splice the variable-width linearization parameter dictionary into `bytes`
/// using the values from `offsets`, then back-patch the `/Prev` field in the
/// Part 1 (first) trailer.
///
/// Behaviour:
/// * The rewritable region runs from `<<` through the trailing pad reserve
///   (the writer stores its absolute span in `offsets.dict_writable_region`).
/// * `<< /Linearized 1 /L V /H [ V V ] /O V /E V /N V /T V >>\nendobj\n` is
///   written at the region start, then the remainder of the region is filled
///   with ASCII space (`b' '`) — total region length is unchanged so every
///   downstream byte offset stays consistent with the writer's probe.
/// * `offsets.part1_placeholders` is updated to point at the new
///   variable-width value bytes inside `bytes` so callers can keep using it
///   to inspect the back-patched values.
/// * `/Prev` is written left-justified, space-padded to `PREV_PLACEHOLDER_WIDTH`.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if any value exceeds 10^10 − 1 (in
/// which case the placeholder reserve cannot guarantee fit), if the rendered
/// dict body is larger than the rewritable region, or if `/Prev` is malformed.
///
/// # Panics
///
/// Does not panic; all bounds and value limits are validated in a preflight
/// pass before any byte is mutated, so an `Err` leaves `bytes` byte-wise
/// unchanged.
pub fn back_patch_param_dict(bytes: &mut [u8], offsets: &mut LinearizedOffsets) -> Result<()> {
    let values = Part1Values::from_offsets(offsets);

    // -----------------------------------------------------------------------
    // Pass 1: preflight value bounds, region bounds, /Prev placeholder.
    // -----------------------------------------------------------------------
    let value_labels = [
        (values.l, "/L"),
        (values.h_offset, "/H[0]"),
        (values.h_length, "/H[1]"),
        (values.o, "/O"),
        (values.e, "/E"),
        (values.n, "/N"),
        (values.t, "/T"),
    ];
    for (v, label) in value_labels {
        if v > MAX_PLACEHOLDER_VALUE {
            return Err(crate::Error::Unsupported(format!(
                "back_patch_param_dict: {label} value {v} exceeds maximum \
                 {PLACEHOLDER_WIDTH}-digit placeholder value ({MAX_PLACEHOLDER_VALUE})",
            )));
        }
    }

    let region = offsets.dict_writable_region.clone();
    if region.end > bytes.len() {
        return Err(crate::Error::Unsupported(format!(
            "back_patch_param_dict: dict_writable_region {:?} out of bounds for \
             buffer length {}",
            region,
            bytes.len()
        )));
    }

    let new_dict = render_compact_dict(&values);
    if new_dict.len() > region.len() {
        return Err(crate::Error::Unsupported(format!(
            "back_patch_param_dict: rendered dict ({} bytes) exceeds reserved \
             dict_writable_region ({} bytes) — Part 1 trailing pad too small",
            new_dict.len(),
            region.len()
        )));
    }

    // Preflight the /Prev range (if non-empty).  Copy the range out so the
    // mutable update of `offsets.part1_placeholders` below does not conflict
    // with an outstanding borrow of `offsets`.
    let prev_range = offsets.first_trailer_prev_range.clone();
    let prev_value = offsets.last_xref_keyword_offset;
    if !prev_range.is_empty() {
        if prev_range.end > bytes.len() {
            return Err(crate::Error::Unsupported(format!(
                "back_patch_param_dict: /Prev placeholder range {prev_range:?} out of bounds for buffer length {}",
                bytes.len()
            )));
        }
        if prev_range.len() != PREV_PLACEHOLDER_WIDTH {
            return Err(crate::Error::Unsupported(format!(
                "back_patch_param_dict: /Prev placeholder range has length {} (expected {PREV_PLACEHOLDER_WIDTH})",
                prev_range.len(),
            )));
        }
        // Value is last_xref_keyword_offset; no overflow check needed for a usize on
        // any realistic PDF file (offset fits in a 22-char decimal string).
    }

    // -----------------------------------------------------------------------
    // Pass 2: splice the new dict body and pad, then update placeholder ranges.
    // -----------------------------------------------------------------------
    let region_start = region.start;
    let region_end = region.end;
    // Write the dict body first.
    bytes[region_start..region_start + new_dict.len()].copy_from_slice(&new_dict);
    // Fill the remainder of the rewritable region with ASCII space — this is
    // the qpdf-style trailing pad that absorbs the bytes saved by compaction.
    bytes[region_start + new_dict.len()..region_end].fill(b' ');

    // Update placeholder ranges to point at the rewritten variable-width
    // value bytes, so callers inspecting `offsets.part1_placeholders` see the
    // current value positions (not the obsolete pre-splice 10-wide slots).
    offsets.part1_placeholders = compute_post_splice_placeholders(region_start, &values);

    // Write the /Prev value (left-justified, space-padded on the right).
    if !prev_range.is_empty() {
        let formatted = format!("{prev_value:<PREV_PLACEHOLDER_WIDTH$}");
        debug_assert_eq!(
            formatted.len(),
            PREV_PLACEHOLDER_WIDTH,
            "formatted /Prev value must be exactly {PREV_PLACEHOLDER_WIDTH} bytes",
        );
        bytes[prev_range].copy_from_slice(formatted.as_bytes());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// LinearizedDocument convenience method
// ---------------------------------------------------------------------------

impl LinearizedDocument {
    /// Back-patch all numeric placeholders in the Part 1 parameter dictionary
    /// with their now-known values from `self.offsets`.
    ///
    /// After this call, `self.bytes` is a complete, valid linearized PDF whose
    /// `/L`, `/H`, `/O`, `/E`, `/T`, and `/N` fields contain the correct values.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`back_patch_param_dict`].
    pub fn back_patch(&mut self) -> Result<()> {
        back_patch_param_dict(&mut self.bytes, &mut self.offsets)
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::part1::{Part1Bytes, PLACEHOLDER_WIDTH};
    use crate::linearization::plan::{LinearizationPlan, PageHintEntry};
    use crate::linearization::renumber::RenumberMap;
    use crate::linearization::writer::{write_linearized, LinearizedDocument, LinearizedOffsets};
    use crate::writer::WriteOptions;
    use crate::{ObjectRef, Pdf};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    // -----------------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------------

    /// Build a minimal single-page PDF (same structure as writer.rs tests).
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

    /// Build a fully linearized document (without back-patching).
    fn build_linearized() -> LinearizedDocument {
        let mut pdf = open_tiny_pdf();
        let plan = LinearizationPlan::from_pdf(&mut pdf).expect("plan");
        let renumber = RenumberMap::from_plan(&plan);
        let mut pdf2 = open_tiny_pdf();
        write_linearized(&plan, &renumber, &mut pdf2, &WriteOptions::default())
            .expect("write_linearized")
    }

    /// Build a `LinearizationPlan` and minimal `Part1Bytes` for standalone tests.
    fn minimal_plan() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(2, 0)],
            part4_rest: vec![ObjectRef::new(1, 0)],
            total_object_count: 3,
            root_ref: Some(ObjectRef::new(1, 0)),
            page_hints: vec![PageHintEntry::placeholder(ObjectRef::new(3, 0))],
            ..Default::default()
        }
    }

    /// Build a minimal `LinearizedOffsets` using a given `Part1Bytes` buffer
    /// (range positions must be real). Values are chosen to fit in 10 digits.
    fn minimal_offsets(part1: &Part1Bytes, file_length: usize) -> LinearizedOffsets {
        LinearizedOffsets {
            file_length,
            hint_stream_offset: 1234,
            hint_stream_length: 567,
            first_page_object_new_num: 2,
            end_of_first_page_offset: 9876,
            last_xref_keyword_offset: 49990,
            last_xref_offset: 50000,
            page_count: 1,
            part1_placeholders: part1.placeholders.clone(),
            xref_offsets: BTreeMap::new(),
            // Empty range — tests that cover /Prev back-patching supply their own.
            first_trailer_prev_range: 0..0,
            dict_writable_region: part1.dict_writable_region.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // 1. back_patch writes correct variable-width values at each updated range
    // -----------------------------------------------------------------------
    #[test]
    fn back_patch_writes_values_at_correct_ranges() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let mut bytes = part1.bytes.clone();
        let file_len = bytes.len() + 99_999; // arbitrary > part1
        let mut offsets = minimal_offsets(&part1, file_len);

        back_patch_param_dict(&mut bytes, &mut offsets).expect("back_patch should succeed");

        let ph = &offsets.part1_placeholders;

        // Each updated range must contain the variable-width decimal of its value.
        let check = |range: &std::ops::Range<usize>, expected: u64, name: &str| {
            let s = std::str::from_utf8(&bytes[range.clone()]).expect("UTF-8");
            let parsed: u64 = s
                .parse()
                .unwrap_or_else(|e| panic!("{name}: '{s}' must be a decimal integer: {e}"));
            assert_eq!(parsed, expected, "{name}: expected {expected}, got '{s}'");
            assert_eq!(
                s,
                expected.to_string(),
                "{name}: must be variable-width (no zero-padding); got '{s}'"
            );
        };

        check(&ph.l, file_len as u64, "/L");
        check(&ph.h_offset, 1234, "/H[0]");
        check(&ph.h_length, 567, "/H[1]");
        check(&ph.o, 2, "/O");
        check(&ph.e, 9876, "/E");
        check(&ph.t, 50000, "/T");
        check(&ph.n, 1, "/N");
    }

    // -----------------------------------------------------------------------
    // 2. Bytes outside the dict_writable_region are not modified
    //
    //    Inside the region the splice rewrites every byte (dict body shrinks,
    //    trailing pad grows), so the regression that matters is "do not touch
    //    anything outside the reserved region".
    // -----------------------------------------------------------------------
    #[test]
    fn back_patch_leaves_bytes_outside_region_unchanged() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let original = part1.bytes.clone();
        let mut bytes = original.clone();
        let mut offsets = minimal_offsets(&part1, bytes.len() + 1);

        back_patch_param_dict(&mut bytes, &mut offsets).expect("back_patch");

        let region = offsets.dict_writable_region.clone();
        // Bytes BEFORE the region (header + `N 0 obj\n`) must be untouched.
        assert_eq!(&bytes[..region.start], &original[..region.start]);
        // Bytes AFTER the region (none in the standalone build, but check the
        // tail just in case the buffer ever grows): must be untouched.
        assert_eq!(&bytes[region.end..], &original[region.end..]);
    }

    // -----------------------------------------------------------------------
    // 3. After back-patching, every updated placeholder range contains only
    //    ASCII digits (no leading zeros, no spaces).
    // -----------------------------------------------------------------------
    #[test]
    fn after_back_patch_placeholders_are_all_digits() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let mut bytes = part1.bytes.clone();
        let mut offsets = minimal_offsets(&part1, bytes.len() + 42);

        back_patch_param_dict(&mut bytes, &mut offsets).expect("back_patch");

        let ph = &offsets.part1_placeholders;
        for range in ph.as_slice() {
            assert!(
                !range.is_empty(),
                "post-splice value range must be non-empty"
            );
            for &b in &bytes[range.clone()] {
                assert!(
                    b.is_ascii_digit(),
                    "byte {b} at position in {range:?} is not an ASCII digit"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // 3b. Trailing-pad region after the rewritten dict is filled with ASCII
    //     space — qpdf's byte format, used to keep Part 1 length constant.
    // -----------------------------------------------------------------------
    #[test]
    fn after_back_patch_trailing_region_is_ascii_space() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let mut bytes = part1.bytes.clone();
        let mut offsets = minimal_offsets(&part1, bytes.len() + 42);

        back_patch_param_dict(&mut bytes, &mut offsets).expect("back_patch");

        // Find the `\nendobj\n` marker — every byte after it (within the
        // dict_writable_region) must be ASCII space.
        let region = &offsets.dict_writable_region;
        let endobj_needle = b"\nendobj\n";
        let endobj_pos = bytes[region.clone()]
            .windows(endobj_needle.len())
            .position(|w| w == endobj_needle)
            .expect("rewritten dict must contain `\\nendobj\\n`");
        let pad_start = region.start + endobj_pos + endobj_needle.len();
        for &b in &bytes[pad_start..region.end] {
            assert_eq!(
                b,
                b' ',
                "trailing pad must be ASCII space; got {b:?} at offset within \
                 [{pad_start}..{end}]",
                end = region.end
            );
        }
    }

    // -----------------------------------------------------------------------
    // 4. Overflow: values ≥ 10^10 return Err
    // -----------------------------------------------------------------------
    #[test]
    fn overflow_file_length_returns_err() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let mut bytes = part1.bytes.clone();

        // file_length = 10^10 (one more than max)
        let mut offsets = minimal_offsets(&part1, 10_000_000_000);
        offsets.file_length = 10_000_000_000; // usize, fine on 64-bit

        let result = back_patch_param_dict(&mut bytes, &mut offsets);
        assert!(
            result.is_err(),
            "back_patch must return Err when /L value overflows 10 digits"
        );
    }

    #[test]
    fn overflow_hint_stream_offset_returns_err() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let mut bytes = part1.bytes.clone();
        let mut offsets = minimal_offsets(&part1, 100);
        offsets.hint_stream_offset = 10_000_000_000;

        let result = back_patch_param_dict(&mut bytes, &mut offsets);
        assert!(result.is_err(), "back_patch must Err on /H[0] overflow");
    }

    #[test]
    fn overflow_last_xref_offset_returns_err() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let mut bytes = part1.bytes.clone();
        let mut offsets = minimal_offsets(&part1, 100);
        offsets.last_xref_offset = 10_000_000_001;

        let result = back_patch_param_dict(&mut bytes, &mut offsets);
        assert!(result.is_err(), "back_patch must Err on /T overflow");
    }

    /// Regression: a value-overflow Err must leave `bytes` byte-wise unchanged
    /// (preflight-then-write contract).  Without preflight, a late-field error
    /// could leave the splice half-applied with an inconsistent dict.
    #[test]
    fn overflow_on_late_field_leaves_bytes_unchanged() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber, "1.4");
        let original = part1.bytes.clone();
        let mut bytes = original.clone();

        // All fields legal except /T (last in the value-label list).
        let mut offsets = minimal_offsets(&part1, 100);
        offsets.file_length = 12345; // would normally write a non-zero /L
        offsets.last_xref_offset = 10_000_000_000; // overflow on a late field

        let result = back_patch_param_dict(&mut bytes, &mut offsets);
        assert!(result.is_err(), "back_patch must Err on /T overflow");
        assert_eq!(
            bytes, original,
            "overflow on a late field must NOT leave the dict half-rewritten \
             (preflight-then-write contract)"
        );
    }

    // -----------------------------------------------------------------------
    // 5. back_patch via LinearizedDocument::back_patch (convenience method)
    // -----------------------------------------------------------------------
    #[test]
    fn linearized_document_back_patch_method_works() {
        let mut doc = build_linearized();
        doc.back_patch().expect("LinearizedDocument::back_patch");

        // /L must now be the actual file length, written as variable-width decimal.
        let expected_l = doc.offsets.file_length.to_string();
        let ph = &doc.offsets.part1_placeholders;
        let actual_l = std::str::from_utf8(&doc.bytes[ph.l.clone()]).expect("UTF-8");
        assert_eq!(
            actual_l, expected_l,
            "/L must be back-patched to file_length (variable-width)"
        );
    }

    // -----------------------------------------------------------------------
    // 6. Byte-level substring: "/L <decimal>" appears in output, no zero pad
    // -----------------------------------------------------------------------
    #[test]
    fn output_contains_l_substring() {
        let mut doc = build_linearized();
        doc.back_patch().expect("back_patch");

        let expected_value = doc.offsets.file_length.to_string();
        let needle = format!("/L {expected_value} ");
        assert!(
            doc.bytes
                .windows(needle.len())
                .any(|w| w == needle.as_bytes()),
            "back-patched bytes must contain '{needle}' (variable-width)"
        );
    }

    // -----------------------------------------------------------------------
    // 7. Round-trip: write_linearized → back_patch → Pdf::open succeeds
    // -----------------------------------------------------------------------
    #[test]
    fn round_trip_back_patch_then_open() {
        let mut doc = build_linearized();
        doc.back_patch().expect("back_patch");

        Pdf::open(Cursor::new(doc.bytes))
            .expect("back-patched linearized PDF must be parseable by Pdf::open");
    }

    // -----------------------------------------------------------------------
    // 8. After back-patch, /T value in bytes matches last_xref_offset
    // -----------------------------------------------------------------------
    #[test]
    fn t_value_matches_last_xref_offset() {
        let mut doc = build_linearized();
        doc.back_patch().expect("back_patch");

        let ph = &doc.offsets.part1_placeholders;
        let t_bytes = &doc.bytes[ph.t.clone()];
        let t_str = std::str::from_utf8(t_bytes).expect("UTF-8");
        let t_val: usize = t_str.parse().expect("/T decimal");
        assert_eq!(
            t_val, doc.offsets.last_xref_offset,
            "/T back-patched value must equal last_xref_offset"
        );
    }

    // -----------------------------------------------------------------------
    // 9. After back-patch, /N value equals page_count
    // -----------------------------------------------------------------------
    #[test]
    fn n_value_matches_page_count() {
        let mut doc = build_linearized();
        doc.back_patch().expect("back_patch");

        let ph = &doc.offsets.part1_placeholders;
        let n_bytes = &doc.bytes[ph.n.clone()];
        let n_str = std::str::from_utf8(n_bytes).expect("UTF-8");
        let n_val: u32 = n_str.parse().expect("/N decimal");
        assert_eq!(n_val, doc.offsets.page_count, "/N must equal page_count");
    }

    // -----------------------------------------------------------------------
    // 10. back_patch is idempotent (calling twice gives same result)
    // -----------------------------------------------------------------------
    #[test]
    fn back_patch_is_idempotent() {
        let mut doc1 = build_linearized();
        doc1.back_patch().expect("first back_patch");
        let after_first = doc1.bytes.clone();

        // Second call on the already-spliced bytes.  Placeholders point at
        // the variable-width values now; the splice should still produce the
        // same dict + same pad → same bytes.
        back_patch_param_dict(&mut doc1.bytes, &mut doc1.offsets).expect("second back_patch");
        assert_eq!(doc1.bytes, after_first, "back_patch must be idempotent");
    }

    // -----------------------------------------------------------------------
    // 11. PLACEHOLDER_WIDTH is the public cap used by the preflight; ensure
    //     it stays the qpdf-spec 10 (so 10^10 stays the value overflow limit).
    // -----------------------------------------------------------------------
    #[test]
    fn placeholder_width_constant_is_ten() {
        assert_eq!(PLACEHOLDER_WIDTH, 10);
    }
}
