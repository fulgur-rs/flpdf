//! Part 1 emitter — file header + linearization parameter dictionary.
//!
//! ISO 32000-1 Annex F §F.2 specifies that the first body section of a
//! linearized PDF contains:
//!
//! 1. The file header (`%PDF-x.y` + binary marker).
//! 2. Object 1 — the linearization parameter dictionary with fixed-width
//!    (10-digit) decimal placeholders for all numeric values that are not
//!    yet known at layout time.
//!
//! # Placeholder discipline
//!
//! Every numeric value in the dict is represented as a **10-digit, zero-padded**
//! ASCII decimal string (e.g. `0000000000`).  This fixed-width encoding means
//! that a back-patcher (sub-task 2.9) can overwrite values in-place without
//! shifting any downstream byte offsets.
//!
//! [`Part1Placeholders`] records the exact byte ranges of each placeholder so
//! the back-patcher can find and replace them without scanning the file.
//!
//! # Scope
//!
//! This module emits **header + object 1 only**.  The Part 1 xref subsection
//! and trailer (required by the linearized format) are written by the full
//! orchestrator in sub-task 2.8.

use std::ops::Range;

use super::plan::LinearizationPlan;
use super::renumber::RenumberMap;

// ---------------------------------------------------------------------------
// Placeholder width
// ---------------------------------------------------------------------------

/// Number of ASCII digits used for every numeric placeholder.
pub const PLACEHOLDER_WIDTH: usize = 10;

/// The placeholder bytes (10 ASCII `b'0'` characters).
const PLACEHOLDER: &[u8] = b"0000000000";

// ---------------------------------------------------------------------------
// Part1Placeholders
// ---------------------------------------------------------------------------

/// Byte ranges of each placeholder inside the [`Part1Bytes`] buffer.
///
/// Each range points to exactly [`PLACEHOLDER_WIDTH`] bytes of ASCII `b'0'`.
/// The back-patcher (sub-task 2.9) writes the true values into these ranges.
///
/// ## Which fields need back-patching
///
/// | Field       | Value source                                  |
/// |-------------|-----------------------------------------------|
/// | `l`         | `/L` — total file length (back-patch)         |
/// | `h_offset`  | `/H[0]` — hint stream byte offset (back-patch)|
/// | `h_length`  | `/H[1]` — hint stream byte length (back-patch)|
/// | `o`         | `/O` — first-page object new number (back-patch) |
/// | `e`         | `/E` — end of first-page section (back-patch) |
/// | `t`         | `/T` — offset of last xref (back-patch)       |
/// | `n`         | `/N` — page count (back-patch)                |
///
/// All seven fields are placeholders for uniform treatment, even though `/O`
/// and `/N` could in principle be computed immediately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part1Placeholders {
    /// `/L` — total file length in bytes.
    pub l: Range<usize>,
    /// `/H[0]` — hint stream byte offset.
    pub h_offset: Range<usize>,
    /// `/H[1]` — hint stream byte length.
    pub h_length: Range<usize>,
    /// `/O` — first-page object new number (typically 2).
    pub o: Range<usize>,
    /// `/E` — end of first-page section offset.
    pub e: Range<usize>,
    /// `/T` — offset of last cross-reference section.
    pub t: Range<usize>,
    /// `/N` — number of pages.
    pub n: Range<usize>,
}

impl Part1Placeholders {
    /// Return all seven ranges as a slice in dict-key order: L, H[0], H[1],
    /// O, E, T, N.
    ///
    /// Useful for checking disjoint and ordering invariants.
    pub fn as_slice(&self) -> [Range<usize>; 7] {
        [
            self.l.clone(),
            self.h_offset.clone(),
            self.h_length.clone(),
            self.o.clone(),
            self.e.clone(),
            self.t.clone(),
            self.n.clone(),
        ]
    }

    /// Returns `true` if all seven placeholder ranges are pairwise disjoint and
    /// each has a width of exactly [`PLACEHOLDER_WIDTH`].
    pub fn all_valid(&self) -> bool {
        let ranges = self.as_slice();
        for r in &ranges {
            if r.len() != PLACEHOLDER_WIDTH {
                return false;
            }
        }
        // Check pairwise disjoint: sort by start and verify no overlap.
        let mut starts: Vec<(usize, usize)> = ranges.iter().map(|r| (r.start, r.end)).collect();
        starts.sort_unstable_by_key(|&(s, _)| s);
        for window in starts.windows(2) {
            if window[0].1 > window[1].0 {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Part1Bytes
// ---------------------------------------------------------------------------

/// The serialized bytes for Part 1 (header + linearization parameter dict)
/// together with the placeholder positions needed for back-patching.
///
/// Construct via [`Part1Bytes::build`].
#[derive(Debug, Clone)]
pub struct Part1Bytes {
    /// Raw bytes: file header followed by object 1.
    pub bytes: Vec<u8>,
    /// Byte positions of every numeric placeholder in `bytes`.
    pub placeholders: Part1Placeholders,
    /// Byte offset within `bytes` of the `1 0 obj` token (i.e. the start of
    /// object 1's body, after the file header).  The linearized writer needs
    /// this for the Part 1 xref subsection's offset entry.
    ///
    /// Exposed instead of having callers hardcode the header length so that
    /// changes to the header format (PDF version bump, binary marker change)
    /// do not silently break downstream xref entries.
    pub obj1_offset: usize,
}

impl Part1Bytes {
    /// Serialize Part 1 from `plan` and `renumber`.
    ///
    /// The output is deterministic: the same inputs always produce the same
    /// bytes.
    ///
    /// # Header
    ///
    /// `%PDF-1.7\n` followed by a binary marker line `%<0xE2 0xE3 0xCF 0xD3>\n`.
    ///
    /// # Object 1 format
    ///
    /// ```text
    /// 1 0 obj
    /// << /Linearized 1 /L XXXXXXXXXX /H [ XXXXXXXXXX XXXXXXXXXX ] /O XXXXXXXXXX /E XXXXXXXXXX /N XXXXXXXXXX /T XXXXXXXXXX >>
    /// endobj
    /// ```
    ///
    /// where each `XXXXXXXXXX` is a 10-digit zero placeholder.
    ///
    /// `pdf_version` is the effective PDF version string to emit in the header
    /// (e.g. `"1.3"`, `"1.7"`).  Callers should compute this via
    /// [`crate::writer::effective_pdf_version`] before calling `build`.
    pub fn build(_plan: &LinearizationPlan, renumber: &RenumberMap, pdf_version: &str) -> Self {
        // The param-dict slot must still hold its sentinel — overwriting it
        // would cause the writer to emit a duplicate object definition on the
        // same number, corrupting the xref. The slot number itself is now
        // dynamic (qpdf places the param dict after the Pages tree / Info
        // promotion), so the writer reads it from the renumber map.
        debug_assert!(
            renumber.param_dict_slot_is_reserved(),
            "Part1Bytes::build: RenumberMap allocated a plan object on top \
             of the param-dict slot ({param_slot}) — slot must stay reserved",
            param_slot = renumber.param_dict_ref().number
        );

        let param_dict_obj_number = renumber.param_dict_ref().number;

        let mut bytes: Vec<u8> = Vec::new();

        // ------------------------------------------------------------------
        // File header
        // ------------------------------------------------------------------
        // Emit the caller-supplied version (computed via effective_pdf_version).
        bytes.extend_from_slice(format!("%PDF-{pdf_version}\n").as_bytes());
        // Binary marker: four bytes >= 128 signals a binary file.
        bytes.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        // ------------------------------------------------------------------
        // Object {param_dict_obj_number}: linearization parameter dictionary
        // ------------------------------------------------------------------
        // Capture the absolute offset of the object header token so callers
        // do not need to compute the file header length out of band.
        let param_dict_offset = bytes.len();
        bytes.extend_from_slice(format!("{param_dict_obj_number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(b"<< /Linearized 1 /L ");

        // /L placeholder
        let l_start = bytes.len();
        bytes.extend_from_slice(PLACEHOLDER);
        let l_end = bytes.len();

        bytes.extend_from_slice(b" /H [ ");

        // /H[0] placeholder — hint stream offset
        let h_offset_start = bytes.len();
        bytes.extend_from_slice(PLACEHOLDER);
        let h_offset_end = bytes.len();

        bytes.extend_from_slice(b" ");

        // /H[1] placeholder — hint stream length
        let h_length_start = bytes.len();
        bytes.extend_from_slice(PLACEHOLDER);
        let h_length_end = bytes.len();

        bytes.extend_from_slice(b" ] /O ");

        // /O placeholder — first-page object new number
        let o_start = bytes.len();
        bytes.extend_from_slice(PLACEHOLDER);
        let o_end = bytes.len();

        bytes.extend_from_slice(b" /E ");

        // /E placeholder — end of first-page section
        let e_start = bytes.len();
        bytes.extend_from_slice(PLACEHOLDER);
        let e_end = bytes.len();

        // qpdf emits the linearization param dict with /N preceding /T
        // (the spec only requires /Linearized to come first; the remaining
        // keys' order is implementation-defined). Match qpdf so writers
        // doing byte comparisons across tools agree.
        bytes.extend_from_slice(b" /N ");

        // /N placeholder — page count
        let n_start = bytes.len();
        bytes.extend_from_slice(PLACEHOLDER);
        let n_end = bytes.len();

        bytes.extend_from_slice(b" /T ");

        // /T placeholder — offset of last xref
        let t_start = bytes.len();
        bytes.extend_from_slice(PLACEHOLDER);
        let t_end = bytes.len();

        bytes.extend_from_slice(b" >>\n");
        bytes.extend_from_slice(b"endobj\n");

        let placeholders = Part1Placeholders {
            l: l_start..l_end,
            h_offset: h_offset_start..h_offset_end,
            h_length: h_length_start..h_length_end,
            o: o_start..o_end,
            e: e_start..e_end,
            t: t_start..t_end,
            n: n_start..n_end,
        };

        Self {
            bytes,
            placeholders,
            obj1_offset: param_dict_offset,
        }
    }

    /// Length of the serialized Part 1 in bytes.
    pub fn byte_length(&self) -> usize {
        self.bytes.len()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::plan::{LinearizationPlan, PageHintEntry};
    use crate::ObjectRef;

    // -----------------------------------------------------------------------
    // Fixture helpers
    // -----------------------------------------------------------------------

    fn minimal_plan() -> LinearizationPlan {
        // Single page, no shared objects.
        LinearizationPlan {
            part2_objects: vec![ObjectRef::new(3, 0), ObjectRef::new(2, 0)],
            part4_rest: vec![ObjectRef::new(1, 0)],
            total_object_count: 3,
            root_ref: Some(ObjectRef::new(1, 0)),
            page_hints: vec![PageHintEntry::placeholder(ObjectRef::new(3, 0))],
            ..Default::default()
        }
    }

    fn build_part1() -> Part1Bytes {
        let plan = minimal_plan();
        let renumber = RenumberMap::from_plan(&plan);
        Part1Bytes::build(&plan, &renumber, "1.4")
    }

    // -----------------------------------------------------------------------
    // 1. byte_length is deterministic
    // -----------------------------------------------------------------------
    #[test]
    fn byte_length_is_deterministic() {
        let p1 = build_part1();
        let p2 = build_part1();
        assert_eq!(
            p1.bytes, p2.bytes,
            "Part 1 must be bytewise identical on repeated calls"
        );
        assert_eq!(p1.byte_length(), p2.byte_length());
    }

    // -----------------------------------------------------------------------
    // 2. Output starts with %PDF- header
    // -----------------------------------------------------------------------
    #[test]
    fn output_starts_with_pdf_header() {
        let p1 = build_part1();
        assert!(
            p1.bytes.starts_with(b"%PDF-"),
            "Part 1 must start with %PDF-"
        );
    }

    // -----------------------------------------------------------------------
    // 3. Output contains "1 0 obj"
    // -----------------------------------------------------------------------
    #[test]
    fn output_contains_object_1_header() {
        let p1 = build_part1();
        assert!(
            p1.bytes.windows(b"1 0 obj".len()).any(|w| w == b"1 0 obj"),
            "Part 1 must contain '1 0 obj'"
        );
    }

    // -----------------------------------------------------------------------
    // 4. Output contains /Linearized 1
    // -----------------------------------------------------------------------
    #[test]
    fn output_contains_linearized_key() {
        let p1 = build_part1();
        let needle = b"/Linearized 1";
        assert!(
            p1.bytes.windows(needle.len()).any(|w| w == needle),
            "Part 1 must contain '/Linearized 1'"
        );
    }

    // -----------------------------------------------------------------------
    // 5. Each placeholder is exactly PLACEHOLDER_WIDTH ASCII '0' bytes
    // -----------------------------------------------------------------------
    #[test]
    fn placeholders_are_ten_zero_digits() {
        let p1 = build_part1();
        let ph = &p1.placeholders;
        for range in ph.as_slice() {
            assert_eq!(
                range.len(),
                PLACEHOLDER_WIDTH,
                "placeholder range {:?} must have width {}",
                range,
                PLACEHOLDER_WIDTH
            );
            assert!(
                p1.bytes[range.clone()].iter().all(|&b| b == b'0'),
                "placeholder bytes at {:?} must all be b'0'",
                range
            );
        }
    }

    // -----------------------------------------------------------------------
    // 6. Placeholder byte ranges match the bytes in the buffer
    // -----------------------------------------------------------------------
    #[test]
    fn placeholder_ranges_point_to_zero_bytes() {
        let p1 = build_part1();
        let ph = &p1.placeholders;

        // Spot-check each named field.
        assert_eq!(&p1.bytes[ph.l.clone()], PLACEHOLDER);
        assert_eq!(&p1.bytes[ph.h_offset.clone()], PLACEHOLDER);
        assert_eq!(&p1.bytes[ph.h_length.clone()], PLACEHOLDER);
        assert_eq!(&p1.bytes[ph.o.clone()], PLACEHOLDER);
        assert_eq!(&p1.bytes[ph.e.clone()], PLACEHOLDER);
        assert_eq!(&p1.bytes[ph.t.clone()], PLACEHOLDER);
        assert_eq!(&p1.bytes[ph.n.clone()], PLACEHOLDER);
    }

    // -----------------------------------------------------------------------
    // 7. All placeholder ranges are disjoint
    // -----------------------------------------------------------------------
    #[test]
    fn placeholder_ranges_are_disjoint() {
        let p1 = build_part1();
        assert!(
            p1.placeholders.all_valid(),
            "Part1Placeholders must be valid (all width={}, all disjoint)",
            PLACEHOLDER_WIDTH
        );
    }

    // -----------------------------------------------------------------------
    // 8. Placeholder ranges appear in ascending order in the buffer
    // -----------------------------------------------------------------------
    #[test]
    fn placeholders_are_in_ascending_order() {
        let p1 = build_part1();
        let ph = &p1.placeholders;
        // qpdf-aligned emission order: /L /H[0] /H[1] /O /E /N /T
        let starts = [
            ph.l.start,
            ph.h_offset.start,
            ph.h_length.start,
            ph.o.start,
            ph.e.start,
            ph.n.start,
            ph.t.start,
        ];
        let names = ["l", "h_offset", "h_length", "o", "e", "n", "t"];
        for i in 1..starts.len() {
            assert!(
                starts[i] > starts[i - 1],
                "placeholder '{}' (start {}) must come after '{}' (start {})",
                names[i],
                starts[i],
                names[i - 1],
                starts[i - 1]
            );
        }
    }

    // -----------------------------------------------------------------------
    // 9. byte_length is positive and consistent with buffer
    // -----------------------------------------------------------------------
    #[test]
    fn byte_length_equals_buffer_len() {
        let p1 = build_part1();
        assert_eq!(p1.byte_length(), p1.bytes.len());
        assert!(p1.byte_length() > 0);
    }

    // -----------------------------------------------------------------------
    // 10. Binary marker line follows the header
    // -----------------------------------------------------------------------
    #[test]
    fn binary_marker_present() {
        let p1 = build_part1();
        // The binary marker bytes follow immediately after the header line.
        // build_part1() uses version "1.4", so the header is "%PDF-1.4\n".
        let expected_marker: &[u8] = b"%\xE2\xE3\xCF\xD3\n";
        let header_len = b"%PDF-1.4\n".len();
        assert!(
            p1.bytes.len() > header_len + expected_marker.len(),
            "buffer too short to contain binary marker"
        );
        assert_eq!(
            &p1.bytes[header_len..header_len + expected_marker.len()],
            expected_marker,
            "binary marker must follow the PDF header"
        );
    }

    // -----------------------------------------------------------------------
    // 11. Placeholders are followed/preceded by valid PDF separators
    //     (no two placeholders adjacent without whitespace)
    // -----------------------------------------------------------------------
    #[test]
    fn placeholder_boundaries_have_separators() {
        let p1 = build_part1();
        let ph = &p1.placeholders;
        // After /H[0] there must be a space before /H[1].
        let byte_between = p1.bytes[ph.h_offset.end];
        assert_eq!(
            byte_between, b' ',
            "there must be a space between /H[0] and /H[1] placeholders"
        );
    }

    // -----------------------------------------------------------------------
    // 12. obj1_offset matches the actual position of "1 0 obj" in bytes
    // -----------------------------------------------------------------------
    #[test]
    fn obj1_offset_matches_token_position() {
        let p1 = build_part1();
        let token = b"1 0 obj";
        let found = p1
            .bytes
            .windows(token.len())
            .position(|w| w == token)
            .expect("Part 1 must contain the `1 0 obj` token");
        assert_eq!(
            p1.obj1_offset, found,
            "Part1Bytes::obj1_offset must point at the start of `1 0 obj`"
        );
        // Also verify the slice at that offset starts with the token
        // (catches the off-by-one case where obj1_offset points one byte
        // past the start).
        assert!(
            p1.bytes[p1.obj1_offset..].starts_with(token),
            "bytes[obj1_offset..] must start with `1 0 obj`"
        );
    }
}
