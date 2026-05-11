//! Back-patcher for Part 1 linearization parameter dictionary (sub-task 2.9).
//!
//! After layout is complete and all byte offsets are known, this module fills
//! in the 10-digit zero-padded decimal placeholders that [`Part1Bytes::build`]
//! left in the linearization parameter dictionary:
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
//! # Placeholder discipline
//!
//! Every placeholder is exactly [`PLACEHOLDER_WIDTH`] (10) ASCII decimal digits.
//! The back-patcher verifies that each value fits within 10 digits before
//! writing, returning [`crate::Error::Unsupported`] if it does not.
//!
//! # Usage
//!
//! ```rust,ignore
//! let mut doc = write_linearized(&plan, &renumber, &mut pdf)?;
//! doc.back_patch()?;
//! // doc.bytes now contains the complete, patched linearized PDF.
//! ```

use crate::linearization::part1::PLACEHOLDER_WIDTH;
use crate::linearization::writer::{LinearizedDocument, LinearizedOffsets};
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
// Public API
// ---------------------------------------------------------------------------

/// Overwrite each placeholder range in `bytes` with the known value from
/// `offsets`, encoded as a 10-digit zero-padded decimal ASCII string.
///
/// # Errors
///
/// Returns [`crate::Error::Unsupported`] if any value exceeds the maximum
/// representable in [`PLACEHOLDER_WIDTH`] decimal digits (i.e. ≥ 10^10).
///
/// # Panics
///
/// Does not panic; placeholder range widths and value bounds are
/// validated in a preflight pass before any byte is mutated, so an `Err`
/// leaves `bytes` byte-wise unchanged.
pub fn back_patch_param_dict(bytes: &mut [u8], offsets: &LinearizedOffsets) -> Result<()> {
    // Defensive guard: all placeholder ranges must be valid before we start.
    if !offsets.part1_placeholders.all_valid() {
        return Err(crate::Error::Unsupported(
            "back_patch_param_dict: Part1Placeholders are invalid (wrong width or overlapping)"
                .to_string(),
        ));
    }

    // Build a list of (range, value) pairs for uniform processing.
    let ph = &offsets.part1_placeholders;
    let patches: &[(&std::ops::Range<usize>, u64, &str)] = &[
        (&ph.l, offsets.file_length as u64, "/L"),
        (&ph.h_offset, offsets.hint_stream_offset as u64, "/H[0]"),
        (&ph.h_length, offsets.hint_stream_length as u64, "/H[1]"),
        (&ph.o, u64::from(offsets.first_page_object_new_num), "/O"),
        (&ph.e, offsets.end_of_first_page_offset as u64, "/E"),
        (&ph.t, offsets.last_xref_offset as u64, "/T"),
        (&ph.n, u64::from(offsets.page_count), "/N"),
    ];

    // -----------------------------------------------------------------------
    // Pass 1: preflight all patches (range bounds + value overflow) before
    // mutating any byte.  This guarantees that an Err leaves `bytes` byte-
    // wise unchanged, so a caller that recovers from the error sees a
    // consistent buffer.
    // -----------------------------------------------------------------------
    for (range, value, key_name) in patches {
        if range.end > bytes.len() {
            return Err(crate::Error::Unsupported(format!(
                "back_patch_param_dict: {} placeholder range {:?} out of bounds for buffer length {}",
                key_name,
                range,
                bytes.len()
            )));
        }
        if *value > MAX_PLACEHOLDER_VALUE {
            return Err(crate::Error::Unsupported(format!(
                "back_patch_param_dict: {} value {} exceeds maximum \
                 {PLACEHOLDER_WIDTH}-digit placeholder value ({})",
                key_name, value, MAX_PLACEHOLDER_VALUE,
            )));
        }
    }

    // -----------------------------------------------------------------------
    // Pass 2: apply all writes.  After Pass 1 every range / value is known
    // good, so this loop cannot fail.
    // -----------------------------------------------------------------------
    for (range, value, _key_name) in patches {
        let formatted = format!("{value:0PLACEHOLDER_WIDTH$}");
        debug_assert_eq!(
            formatted.len(),
            PLACEHOLDER_WIDTH,
            "formatted value '{formatted}' must be exactly {PLACEHOLDER_WIDTH} bytes",
        );
        bytes[(*range).clone()].copy_from_slice(formatted.as_bytes());
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
        back_patch_param_dict(&mut self.bytes, &self.offsets)
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
        write_linearized(&plan, &renumber, &mut pdf2).expect("write_linearized")
    }

    /// Build a `LinearizationPlan` and minimal `Part1Bytes` for standalone tests.
    fn minimal_plan() -> LinearizationPlan {
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(2, 0)],
            part4_objects: vec![ObjectRef::new(1, 0)],
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
        }
    }

    // -----------------------------------------------------------------------
    // 1. back_patch writes correct 10-digit values at each placeholder range
    // -----------------------------------------------------------------------
    #[test]
    fn back_patch_writes_values_at_correct_ranges() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber);
        let mut bytes = part1.bytes.clone();
        let file_len = bytes.len() + 99_999; // arbitrary > part1
        let offsets = minimal_offsets(&part1, file_len);

        back_patch_param_dict(&mut bytes, &offsets).expect("back_patch should succeed");

        let ph = &offsets.part1_placeholders;

        // Each range must now contain the 10-digit formatted value.
        let check = |range: &std::ops::Range<usize>, expected: u64, name: &str| {
            let s = std::str::from_utf8(&bytes[range.clone()]).expect("UTF-8");
            let parsed: u64 = s.trim_start_matches('0').parse().unwrap_or(0);
            assert_eq!(parsed, expected, "{name}: expected {expected}, got '{s}'");
            assert_eq!(
                s.len(),
                PLACEHOLDER_WIDTH,
                "{name}: length must be {PLACEHOLDER_WIDTH}"
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
    // 2. Bytes outside placeholder ranges are not modified
    // -----------------------------------------------------------------------
    #[test]
    fn back_patch_leaves_non_placeholder_bytes_unchanged() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber);
        let original = part1.bytes.clone();
        let mut bytes = original.clone();
        let offsets = minimal_offsets(&part1, bytes.len() + 1);

        back_patch_param_dict(&mut bytes, &offsets).expect("back_patch");

        let ph = &offsets.part1_placeholders;
        let mut all_ranges: Vec<std::ops::Range<usize>> = ph.as_slice().to_vec();
        all_ranges.sort_by_key(|r| r.start);

        // Collect all byte positions covered by placeholder ranges.
        let placeholder_positions: std::collections::HashSet<usize> =
            all_ranges.iter().flat_map(|r| r.clone()).collect();

        // Every byte NOT in a placeholder must be identical to the original.
        for (i, (&orig, &patched)) in original.iter().zip(bytes.iter()).enumerate() {
            if !placeholder_positions.contains(&i) {
                assert_eq!(
                    orig, patched,
                    "byte at index {i} (outside placeholders) was modified"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // 3. After back-patching, all placeholder ranges contain only digit bytes
    // -----------------------------------------------------------------------
    #[test]
    fn after_back_patch_placeholders_are_all_digits() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber);
        let mut bytes = part1.bytes.clone();
        let offsets = minimal_offsets(&part1, bytes.len() + 42);

        back_patch_param_dict(&mut bytes, &offsets).expect("back_patch");

        let ph = &offsets.part1_placeholders;
        for range in ph.as_slice() {
            for &b in &bytes[range.clone()] {
                assert!(
                    b.is_ascii_digit(),
                    "byte {b} at position in {range:?} is not an ASCII digit"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // 4. Overflow: values ≥ 10^10 return Err
    // -----------------------------------------------------------------------
    #[test]
    fn overflow_file_length_returns_err() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber);
        let mut bytes = part1.bytes.clone();

        // file_length = 10^10 (one more than max)
        let mut offsets = minimal_offsets(&part1, 10_000_000_000);
        offsets.file_length = 10_000_000_000; // usize, fine on 64-bit

        let result = back_patch_param_dict(&mut bytes, &offsets);
        assert!(
            result.is_err(),
            "back_patch must return Err when /L value overflows 10 digits"
        );
    }

    #[test]
    fn overflow_hint_stream_offset_returns_err() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber);
        let mut bytes = part1.bytes.clone();
        let mut offsets = minimal_offsets(&part1, 100);
        offsets.hint_stream_offset = 10_000_000_000;

        let result = back_patch_param_dict(&mut bytes, &offsets);
        assert!(result.is_err(), "back_patch must Err on /H[0] overflow");
    }

    #[test]
    fn overflow_last_xref_offset_returns_err() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber);
        let mut bytes = part1.bytes.clone();
        let mut offsets = minimal_offsets(&part1, 100);
        offsets.last_xref_offset = 10_000_000_001;

        let result = back_patch_param_dict(&mut bytes, &offsets);
        assert!(result.is_err(), "back_patch must Err on /T overflow");
    }

    /// Regression: when a late field overflows, no earlier placeholder
    /// should have been written (preflight-then-write contract).
    #[test]
    fn overflow_on_late_field_leaves_bytes_unchanged() {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        let part1 = Part1Bytes::build(&plan, &renumber);
        let original = part1.bytes.clone();
        let mut bytes = original.clone();

        // All fields legal except /T (sixth in the patch list).
        let mut offsets = minimal_offsets(&part1, 100);
        offsets.file_length = 12345; // would normally write a non-zero /L
        offsets.last_xref_offset = 10_000_000_000; // overflow on a late field

        let result = back_patch_param_dict(&mut bytes, &offsets);
        assert!(result.is_err(), "back_patch must Err on /T overflow");
        assert_eq!(
            bytes, original,
            "overflow on a late field must NOT leave earlier placeholders mutated \
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

        // /L must now be the actual file length (10 digits, zero-padded).
        let expected_l = format!("{:010}", doc.offsets.file_length);
        let ph = &doc.offsets.part1_placeholders;
        let actual_l = std::str::from_utf8(&doc.bytes[ph.l.clone()]).expect("UTF-8");
        assert_eq!(
            actual_l, expected_l,
            "/L must be back-patched to file_length"
        );
    }

    // -----------------------------------------------------------------------
    // 6. Byte-level substring: "/L 0000NNNNNN" appears in output
    // -----------------------------------------------------------------------
    #[test]
    fn output_contains_l_substring() {
        let mut doc = build_linearized();
        doc.back_patch().expect("back_patch");

        let expected_value = format!("{:010}", doc.offsets.file_length);
        let needle = format!("/L {}", expected_value);
        assert!(
            doc.bytes
                .windows(needle.len())
                .any(|w| w == needle.as_bytes()),
            "back-patched bytes must contain '{needle}'"
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
        let t_val: usize = t_str.trim_start_matches('0').parse().unwrap_or(0);
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
        let n_val: u32 = n_str.trim_start_matches('0').parse().unwrap_or(0);
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

        // Second call on the already-patched bytes.
        // (offsets.part1_placeholders still refers to the same byte positions.)
        back_patch_param_dict(&mut doc1.bytes, &doc1.offsets).expect("second back_patch");
        assert_eq!(doc1.bytes, after_first, "back_patch must be idempotent");
    }
}
