//! qpdf correspondence: QPDFJob.cc page-range parsing split from page-operation orchestration.
//! Page-range value owned by qpdf's `QUtil::parse_numrange` primitive.
//!
//! # Syntax
//!
//! ```text
//! range        ::= "" | group ("," group)* (":odd" | ":even")?
//! group        ::= "x"? endpoint ("-" endpoint)?
//! endpoint     ::= "z" | "r" digit+ | digit+
//! ```
//!
//! - `z` — the last page (equivalent to `r1`).
//! - `rN` — N-th page from the end; `r1` is the last page, `r2` is the second-to-last, …
//! - Ranges may be ascending (`1-5`) or descending (`5-1`); both are inclusive.
//! - `x` prepends an exclusion group. It removes the group's pages from the
//!   immediately preceding positive group, matching qpdf's
//!   `QUtil::parse_numrange` (`QUtil.cc:1304-1429`). The first group may not be
//!   an exclusion.
//! - `:odd` / `:even` filter the *positions* in the final expanded selection:
//!   `:odd` keeps positions 1, 3, 5, … (1-based); `:even` keeps positions 2,
//!   4, 6, …. They are not based on the original page numbers. Example:
//!   `2-8:even` → `[3,5,7]`.
//! - `PageRange::parse` validates syntax with qpdf's `max == 0` mode; an empty
//!   expression therefore selects no pages.
//! - `PageRange::all` represents qpdf's page-spec default `1-z`.
//! - Multiple entries are concatenated; the final resolved list preserves
//!   duplicates in declaration order (qpdf-parity: `1,3,1` yields `[1,3,1]`).

use crate::{Error, Result};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Legacy structured endpoint vocabulary for a page-range entry.
///
/// The canonical [`PageRange`] route now retains the raw qpdf expression and
/// delegates both parsing and resolution to [`crate::qutil::parse_numrange`].
/// This public vocabulary remains as a separate visibility/API cleanup surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// Absolute 1-based page number. Must be ≥ 1.
    Num(u32),
    /// The last page (`z`).
    Z,
    /// N-th page from the end (`rN`); `r1` = last, `r2` = second-to-last.
    /// Must be ≥ 1.
    FromEnd(u32),
}

/// Legacy structured representation of a final-position filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    /// Keep positions 1, 3, 5, … (`:odd`).
    Odd,
    /// Keep positions 2, 4, 6, … (`:even`).
    Even,
}

/// Legacy structured representation of one page-range group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRangeEntry {
    /// Whether this group excludes pages from the preceding positive group.
    pub exclude: bool,
    /// Start of the range (or the single page).
    pub start: Endpoint,
    /// End of the range, if this is a range rather than a single page.
    pub end: Option<Endpoint>,
    /// Optional final parity suffix retained on the last entry.
    pub parity: Option<Parity>,
}

/// A qpdf page-range expression, ready to be resolved against a page count.
///
/// Constructed via [`PageRange::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRange {
    /// The original qpdf range expression, retained so parse and resolve both
    /// invoke the same canonical primitive with their respective bounds.
    raw: Vec<u8>,
}

impl PageRange {
    /// Parse a page-range expression.
    ///
    /// Syntax validation uses qpdf's `QUtil::parse_numrange(range, 0)` mode,
    /// which intentionally accepts values whose page bounds cannot be checked
    /// until [`PageRange::resolve`] knows the document page count.
    ///
    /// # Errors
    ///
    /// - The qpdf-compatible range error returned by
    ///   [`crate::qutil::parse_numrange`] when syntax is invalid.
    pub fn parse(input: &str) -> Result<Self> {
        crate::qutil::parse_numrange(input.as_bytes(), 0)?;
        Ok(Self {
            raw: input.as_bytes().to_vec(),
        })
    }

    /// Construct qpdf's default page selection (`1-z`).
    ///
    /// qpdf's page-spec job boundary replaces an omitted range with `1-z`
    /// before resolving it (`libqpdf/QPDFJob.cc:2364-2372`). This constructor
    /// keeps that default distinct from an explicitly empty range, which
    /// selects no pages.
    pub fn all() -> Self {
        Self {
            raw: b"1-z".to_vec(),
        }
    }

    /// Construct a range that selects **no** pages.
    ///
    /// This corresponds to an explicitly empty overlay/underlay range. It is
    /// equivalent to [`PageRange::parse`] with an empty string; qpdf's omitted
    /// page-spec default is represented separately by [`PageRange::all`].
    pub fn empty() -> Self {
        Self { raw: Vec::new() }
    }

    /// Resolve the parsed expression against `page_count` (the number of pages
    /// in the document, ≥ 1).
    ///
    /// Returns a `Vec<u32>` of 1-based page numbers in qpdf declaration order,
    /// preserving duplicates.
    ///
    /// # Errors
    ///
    /// - `page_count` is 0.
    /// - The qpdf-compatible numeric-range error when an endpoint exceeds the
    ///   page count.
    pub fn resolve(&self, page_count: u32) -> Result<Vec<u32>> {
        if page_count == 0 {
            return Err(Error::parse(0, "page_count must be at least 1"));
        }
        let max = crate::qutil::qpdf_size_to_int(page_count as usize)?;
        // qutil enforces 1 <= page <= max for a positive max, and max is
        // already narrowed to i32 above, so this conversion is lossless.
        Ok(crate::qutil::parse_numrange(&self.raw, max)?
            .into_iter()
            .map(|page| page as u32)
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Parse-level tests (qpdf max=0 syntax-only mode)
    // -----------------------------------------------------------------------

    #[test]
    fn parse_accepts_qpdf_syntax_only_values() {
        // QPDFJob validates a page range with QUtil::parse_numrange(..., 0),
        // so numeric bounds are intentionally deferred until resolve.
        assert!(PageRange::parse("0").is_ok());
        assert!(PageRange::parse("r0").is_ok());
        assert!(PageRange::parse("99").is_ok());
        assert!(PageRange::parse(":odd").is_ok());
    }

    #[test]
    fn parse_rejects_only_qpdf_invalid_syntax() {
        for input in ["1-", "-1", "1,,2", "1,2,", "1-9:foo", "abc", "r"] {
            assert!(
                PageRange::parse(input).is_err(),
                "{input:?} must be invalid"
            );
        }
    }

    #[test]
    fn resolve_preserves_qpdf_numeric_range_error_bytes() {
        let range = PageRange::parse("0").expect("qpdf syntax-only parse accepts zero");
        let error = range
            .resolve(3)
            .expect_err("zero is out of range for three pages");
        assert_eq!(
            error.raw_message(),
            Some(b"error at * in numeric range *0: number 0 out of range".as_slice())
        );

        let range = PageRange::parse("r0").expect("qpdf syntax-only parse accepts r0");
        let error = range
            .resolve(3)
            .expect_err("r0 resolves beyond the last page");
        assert_eq!(
            error.raw_message(),
            Some(b"error at * in numeric range *r0: number 4 out of range".as_slice())
        );
    }

    #[test]
    fn parse_empty_is_the_qpdf_empty_selection() {
        let range = PageRange::parse("").expect("empty qpdf range is valid syntax");
        assert_eq!(range.resolve(3).unwrap(), Vec::<u32>::new());
    }

    // -----------------------------------------------------------------------
    // Resolve-level tests
    // -----------------------------------------------------------------------

    fn resolve(input: &str, page_count: u32) -> Vec<u32> {
        PageRange::parse(input)
            .and_then(|pr| pr.resolve(page_count))
            .unwrap_or_else(|e| {
                panic!("expected Ok for {input:?} with {page_count} pages, got: {e}")
            })
    }

    fn resolve_err(input: &str, page_count: u32) -> String {
        let pr = PageRange::parse(input).expect("parse should succeed");
        pr.resolve(page_count)
            .err()
            .unwrap_or_else(|| panic!("expected Err for {input:?} with {page_count} pages"))
            .to_string()
    }

    #[test]
    fn all_constructor_resolves_to_all() {
        assert_eq!(PageRange::all().resolve(5).unwrap(), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn empty_constructor_selects_no_pages() {
        // `PageRange::empty()` is the explicit empty source set, while qpdf's
        // omitted page-spec default is represented by `PageRange::all()`.
        let none = PageRange::empty();
        assert_eq!(none.resolve(5).unwrap(), Vec::<u32>::new());
        assert_eq!(none, PageRange::parse("").unwrap());
        assert_ne!(none, PageRange::all());
    }

    #[test]
    fn single_page_resolve() {
        assert_eq!(resolve("3", 10), vec![3]);
    }

    #[test]
    fn z_resolves_to_last() {
        assert_eq!(resolve("z", 7), vec![7]);
    }

    #[test]
    fn r1_resolves_to_last() {
        assert_eq!(resolve("r1", 7), vec![7]);
    }

    #[test]
    fn r2_resolves_to_second_to_last() {
        assert_eq!(resolve("r2", 7), vec![6]);
    }

    #[test]
    fn ascending_range_resolve() {
        assert_eq!(resolve("1-5", 10), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn descending_range_resolve() {
        assert_eq!(resolve("5-1", 10), vec![5, 4, 3, 2, 1]);
    }

    #[test]
    fn r3_to_z_resolve() {
        // r3 on a 5-page doc = page 3; z = page 5.
        assert_eq!(resolve("r3-z", 5), vec![3, 4, 5]);
    }

    #[test]
    fn odd_parity_resolve() {
        // '1-9:odd' → positions 1,3,5,7,9 of [1..9] = [1,3,5,7,9]
        assert_eq!(resolve("1-9:odd", 9), vec![1, 3, 5, 7, 9]);
    }

    #[test]
    fn even_parity_resolve() {
        // '1-9:even' → positions 2,4,6,8 of [1..9] = [2,4,6,8]
        assert_eq!(resolve("1-9:even", 9), vec![2, 4, 6, 8]);
    }

    #[test]
    fn parity_applies_to_the_whole_selection_not_each_group() {
        // qpdf strips the suffix once and filters the concatenated result
        // (`QUtil.cc:1348-1360` then `:1400-1414`), so the parity positions run
        // across group boundaries. Probed with qpdf 11.9.0 on a 10-page file
        // (`--pages . <range> --`, page identified by its /MediaBox width):
        // `1-3,5-7:odd` -> 1 3 6, `1-3,5-7:even` -> 2 5 7.
        assert_eq!(resolve("1-3,5-7:odd", 10), vec![1, 3, 6]);
        assert_eq!(resolve("1-3,5-7:even", 10), vec![2, 5, 7]);
    }

    #[test]
    fn parity_is_applied_after_exclusion_groups() {
        // qpdf 11.9.0 on the same 10-page file: `1-6,x3:odd` -> 1 4 6.
        assert_eq!(resolve("1-6,x3:odd", 10), vec![1, 4, 6]);
    }

    #[test]
    fn even_parity_offset_range() {
        // '2-8:even' → positions 2,4,6 of [2,3,4,5,6,7,8] = [3,5,7]
        assert_eq!(resolve("2-8:even", 10), vec![3, 5, 7]);
    }

    #[test]
    fn one_to_twenty_even() {
        // '1-20:even' → positions 2,4,...,20 of [1..20] = [2,4,...,20]
        let expected: Vec<u32> = (1..=10).map(|i| i * 2).collect();
        assert_eq!(resolve("1-20:even", 20), expected);
    }

    #[test]
    fn duplicates_preserved_in_declaration_order() {
        // '1,3,5,3' → [1,3,5,3] (qpdf-parity: no dedup).
        // Verified against qpdf 11.9.0: `qpdf --pages in 1,3,5,3 --` emits 4 pages.
        assert_eq!(resolve("1,3,5,3", 10), vec![1, 3, 5, 3]);
    }

    #[test]
    fn repeated_slot_preserved() {
        // '1,1,1,1' → [1,1,1,1] (qpdf-parity: no dedup).
        // This is the case that unblocks overlay --to=1,1,1,1 --from=1-4:
        // the four repeated slots must reach map_overlay_pages so it can pair
        // each slot with the i-th --from source page.
        assert_eq!(resolve("1,1,1,1", 5), vec![1, 1, 1, 1]);
    }

    #[test]
    fn mixed_selection() {
        // '1,3,5-9,15-12' on a 20-page doc
        let result = resolve("1,3,5-9,15-12", 20);
        let expected = vec![1, 3, 5, 6, 7, 8, 9, 15, 14, 13, 12];
        assert_eq!(result, expected);
    }

    #[test]
    fn exclusion_group_removes_pages_from_the_previous_group() {
        // qpdf's `1-3,x2` keeps the first and third pages. The `x` group is
        // applied to the immediately preceding positive group, not parsed as
        // a filename or as a second independent selection.
        assert_eq!(resolve("1-3,x2", 3), vec![1, 3]);
    }

    #[test]
    fn exclusion_group_may_not_be_the_first_group() {
        let message = PageRange::parse("x2").unwrap_err().to_string();
        assert!(
            message.contains("first") || message.contains("exclusion"),
            "got: {message}"
        );
    }

    #[test]
    fn out_of_range_page_number_is_error() {
        let msg = resolve_err("10", 5);
        assert!(msg.contains("out of range"), "got: {msg}");
    }

    #[test]
    fn out_of_range_from_end_is_error() {
        let msg = resolve_err("r10", 5);
        assert!(msg.contains("out of range"), "got: {msg}");
    }

    #[test]
    fn page_count_zero_is_error() {
        let pr = PageRange::parse("1").unwrap();
        let err = pr.resolve(0).unwrap_err().to_string();
        assert!(err.contains("page_count must be at least 1"), "got: {err}");
    }

    #[test]
    fn z_with_single_page_doc() {
        assert_eq!(resolve("z", 1), vec![1]);
    }

    #[test]
    fn full_doc_range_z_1() {
        // 'z-1' on a 5-page doc = [5,4,3,2,1]
        assert_eq!(resolve("z-1", 5), vec![5, 4, 3, 2, 1]);
    }
}
