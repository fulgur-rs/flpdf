//! qpdf correspondence: QPDFJob.cc page-range parsing split from page-operation orchestration.
//! Page-range syntax parser, matching qpdf's page-range mini-language.
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
//!   `QUtil::parse_numrange` (`QUtil.cc:1304-1415`). The first group may not be
//!   an exclusion.
//! - `:odd` / `:even` filter the *positions* in the final expanded selection:
//!   `:odd` keeps positions 1, 3, 5, … (1-based); `:even` keeps positions 2,
//!   4, 6, …. They are not based on the original page numbers. Example:
//!   `2-8:even` → `[3,5,7]`.
//! - An empty string means "all pages" and is resolved to `1..=page_count`.
//! - Multiple entries are concatenated; the final resolved list preserves
//!   duplicates in declaration order (qpdf-parity: `1,3,1` yields `[1,3,1]`,
//!   verified against qpdf 11.9.0 with `qpdf --pages in 1,3,1 --`). Callers
//!   that want a deduplicated set can build one from the returned vector.

use crate::{Error, Result};
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// An unresolved endpoint of a page-range entry.
///
/// Concrete page numbers are not computed until [`PageRange::resolve`] is
/// called with a known `page_count`, so that the parser does not need to know
/// the document's page count.
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

/// Which positions to keep within the final expanded page selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    /// Keep positions 1, 3, 5, … (`:odd`).
    Odd,
    /// Keep positions 2, 4, 6, … (`:even`).
    Even,
}

/// A single parsed entry in a page-range expression.
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

/// A parsed page-range expression, ready to be resolved against a page count.
///
/// Constructed via [`PageRange::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRange {
    /// `None` means "all pages" (empty string input).
    pub(crate) entries: Option<Vec<PageRangeEntry>>,
    /// Optional final-position parity filter.
    parity: Option<Parity>,
}

impl PageRange {
    /// Parse a page-range expression.
    ///
    /// An empty string returns the "all pages" sentinel. Any syntax error
    /// produces an [`Error::Parse`] with an actionable message; the byte
    /// offset is relative to the start of the input string.
    ///
    /// # Errors
    ///
    /// - [`Error::Parse`] when the input is a non-empty string that is not a
    ///   valid page-range expression (invalid endpoint, `0`/`r0`, a dangling
    ///   `-`, an empty or trailing entry, an unknown `:` parity suffix, or any
    ///   unexpected character).
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            return Ok(Self {
                entries: None,
                parity: None,
            });
        }
        let (range, parity) = split_parity_suffix(input)?;
        // qpdf truncates the input at the parity suffix (`range_end = p`,
        // `QUtil.cc:1356`) and then runs `while (p != range_end)`
        // (`QUtil.cc:1367`) zero times, returning an empty result without an
        // error. So ":odd" and ":even" are valid and select nothing.
        if range.is_empty() {
            return Ok(Self {
                entries: Some(Vec::new()),
                parity,
            });
        }
        let mut entries = parse_entries(range)?;
        if let Some(parity) = parity {
            if let Some(entry) = entries.last_mut() {
                // Keep the suffix visible through the existing public entry
                // shape while applying it to the final result in `resolve`.
                entry.parity = Some(parity);
            }
        }
        Ok(Self {
            entries: Some(entries),
            parity,
        })
    }

    /// Construct a range that selects **no** pages.
    ///
    /// This is distinct from [`PageRange::parse`] of the empty string, which
    /// returns the "all pages" sentinel. It corresponds to qpdf's
    /// overlay/underlay semantics where an explicitly empty `--from=` selects an
    /// empty source set, so [`resolve`](PageRange::resolve) returns an empty
    /// vector for any page count.
    pub fn empty() -> Self {
        Self {
            entries: Some(Vec::new()),
            parity: None,
        }
    }

    /// Resolve the parsed expression against `page_count` (the number of pages
    /// in the document, ≥ 1).
    ///
    /// Returns a `Vec<u32>` of 1-based page numbers in declaration order,
    /// preserving duplicates (qpdf-parity: `1,3,1` yields `[1,3,1]`, not
    /// `[1,3]`). Callers that need a deduplicated set can build one from the
    /// returned vector.
    ///
    /// # Errors
    ///
    /// - `page_count` is 0.
    /// - An absolute page number exceeds `page_count`.
    /// - A `rN` endpoint has N > `page_count`.
    /// - A `z` endpoint when `page_count` is 0 (covered by the first check).
    pub fn resolve(&self, page_count: u32) -> Result<Vec<u32>> {
        if page_count == 0 {
            return Err(Error::parse(0, "page_count must be at least 1"));
        }
        let entries = match &self.entries {
            None => return Ok((1..=page_count).collect()),
            Some(e) => e,
        };

        let mut result: Vec<u32> = Vec::new();
        let mut last_group: Vec<u32> = Vec::new();
        for entry in entries {
            let group = resolve_entry(entry, page_count)?;
            if entry.exclude {
                let exclusions = group.into_iter().collect::<BTreeSet<_>>();
                last_group.retain(|page| !exclusions.contains(page));
            } else {
                result.extend(last_group);
                last_group = group;
            }
        }
        result.extend(last_group);

        if let Some(parity) = self.parity {
            let start = match parity {
                Parity::Odd => 0,
                Parity::Even => 1,
            };
            result = result.into_iter().skip(start).step_by(2).collect();
        }
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn resolve_endpoint(ep: &Endpoint, page_count: u32) -> Result<u32> {
    match ep {
        Endpoint::Num(n) => {
            if *n > page_count {
                return Err(Error::parse(
                    0,
                    format!("page number {n} is out of range (document has {page_count} page(s))"),
                ));
            }
            Ok(*n)
        }
        Endpoint::Z => Ok(page_count),
        Endpoint::FromEnd(n) => {
            if *n > page_count {
                return Err(Error::parse(
                    0,
                    format!("r{n} is out of range (document has {page_count} page(s))"),
                ));
            }
            Ok(page_count + 1 - n)
        }
    }
}

fn resolve_entry(entry: &PageRangeEntry, page_count: u32) -> Result<Vec<u32>> {
    let start = resolve_endpoint(&entry.start, page_count)?;
    let end = match &entry.end {
        None => start,
        Some(ep) => resolve_endpoint(ep, page_count)?,
    };

    // Build the sequence (ascending or descending).
    let seq: Vec<u32> = if start <= end {
        (start..=end).collect()
    } else {
        (end..=start).rev().collect()
    };

    Ok(seq)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct RangeParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> RangeParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn remaining(&self) -> &str {
        &self.input[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        Error::parse(self.pos, msg.into())
    }

    /// Parse a non-negative integer. Returns Err if no digits are found.
    fn parse_u32(&mut self) -> Result<u32> {
        let start = self.pos;
        let digits: String = self
            .remaining()
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            return Err(self.err(format!("expected a page number at position {}", self.pos)));
        }
        self.advance(digits.len());
        digits
            .parse::<u32>()
            .map_err(|_| Error::parse(start, format!("page number too large at position {start}")))
    }

    /// Parse a single endpoint: `z`, `rN`, or a positive integer.
    fn parse_endpoint(&mut self) -> Result<Endpoint> {
        match self.peek() {
            Some('z') => {
                self.advance(1);
                Ok(Endpoint::Z)
            }
            Some('r') => {
                self.advance(1);
                let n = self.parse_u32()?;
                if n == 0 {
                    return Err(self.err("r0 is invalid; r1 means the last page"));
                }
                Ok(Endpoint::FromEnd(n))
            }
            Some(c) if c.is_ascii_digit() => {
                let start_pos = self.pos;
                let n = self.parse_u32()?;
                if n == 0 {
                    return Err(Error::parse(
                        start_pos,
                        "page number 0 is invalid; pages are 1-based",
                    ));
                }
                Ok(Endpoint::Num(n))
            }
            Some(c) => Err(self.err(format!(
                "unexpected character '{c}' at position {}; expected a page number, 'z', or 'rN'",
                self.pos
            ))),
            None => Err(self.err(format!(
                "unexpected end of input at position {}; expected a page number, 'z', or 'rN'",
                self.pos
            ))),
        }
    }

    /// Parse one group: `[x]endpoint ("-" endpoint)?`.
    fn parse_entry(&mut self) -> Result<PageRangeEntry> {
        let exclude = if self.peek() == Some('x') {
            self.advance(1);
            true
        } else {
            false
        };
        let start = self.parse_endpoint()?;

        let end = if self.remaining().starts_with('-') {
            self.advance(1);
            Some(self.parse_endpoint()?)
        } else {
            None
        };

        Ok(PageRangeEntry {
            exclude,
            start,
            end,
            parity: None,
        })
    }
}

fn split_parity_suffix(input: &str) -> Result<(&str, Option<Parity>)> {
    // qpdf takes the FIRST colon (`std::find`, `QUtil.cc:1348`) and then
    // requires the remainder to be exactly ":odd" or ":even" (`strcmp`,
    // `QUtil.cc:1350-1356`).
    let Some(index) = input.find(':') else {
        return Ok((input, None));
    };
    let (range, suffix) = input.split_at(index);
    let parity = match suffix {
        ":odd" => Parity::Odd,
        ":even" => Parity::Even,
        _ => {
            return Err(Error::parse(
                index,
                format!(
                "unknown parity suffix '{suffix}' at position {index}; expected ':odd' or ':even'"
            ),
            ))
        }
    };
    Ok((range, Some(parity)))
}

fn parse_entries(input: &str) -> Result<Vec<PageRangeEntry>> {
    let mut p = RangeParser::new(input);
    let mut entries = Vec::new();

    loop {
        // Empty segment (e.g. "1,,2" would have an empty segment between commas).
        match p.peek() {
            None => break,
            Some(',') => {
                return Err(p.err(format!(
                    "empty entry at position {}; consecutive commas are not allowed",
                    p.pos
                )));
            }
            _ => {}
        }

        if entries.is_empty() && p.peek() == Some('x') {
            return Err(p.err("first range group may not be an exclusion"));
        }
        entries.push(p.parse_entry()?);

        match p.peek() {
            None => break,
            Some(',') => {
                p.advance(1);
                // Check for trailing comma.
                if p.peek().is_none() {
                    return Err(p.err(format!("trailing comma at position {}", p.pos)));
                }
            }
            Some(c) => {
                return Err(p.err(format!(
                    "unexpected character '{c}' at position {}; expected ',' or end of input",
                    p.pos
                )));
            }
        }
    }

    if entries.is_empty() {
        // Should not reach here since we check `input.is_empty()` above,
        // but guard defensively.
        return Err(Error::parse(0, "page range is empty"));
    }

    Ok(entries)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Parse-level tests (no page count needed)
    // -----------------------------------------------------------------------

    fn parse_ok(input: &str) -> PageRange {
        PageRange::parse(input).unwrap_or_else(|e| panic!("expected Ok for {input:?}, got: {e}"))
    }

    fn parse_err(input: &str) -> String {
        PageRange::parse(input)
            .err()
            .unwrap_or_else(|| panic!("expected Err for {input:?}"))
            .to_string()
    }

    #[test]
    fn empty_string_is_all_pages() {
        let pr = parse_ok("");
        assert_eq!(pr.entries, None);
    }

    #[test]
    fn single_page() {
        let pr = parse_ok("3");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].start, Endpoint::Num(3));
        assert_eq!(entries[0].end, None);
        assert_eq!(entries[0].parity, None);
    }

    #[test]
    fn z_endpoint() {
        let pr = parse_ok("z");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::Z);
    }

    #[test]
    fn from_end_endpoint() {
        let pr = parse_ok("r3");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::FromEnd(3));
    }

    #[test]
    fn ascending_range() {
        let pr = parse_ok("1-5");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::Num(1));
        assert_eq!(entries[0].end, Some(Endpoint::Num(5)));
    }

    #[test]
    fn descending_range() {
        let pr = parse_ok("5-1");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::Num(5));
        assert_eq!(entries[0].end, Some(Endpoint::Num(1)));
    }

    #[test]
    fn range_with_z() {
        let pr = parse_ok("r3-z");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::FromEnd(3));
        assert_eq!(entries[0].end, Some(Endpoint::Z));
    }

    #[test]
    fn odd_parity() {
        let pr = parse_ok("1-9:odd");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].parity, Some(Parity::Odd));
    }

    #[test]
    fn even_parity() {
        let pr = parse_ok("1-9:even");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].parity, Some(Parity::Even));
    }

    #[test]
    fn multiple_entries() {
        let pr = parse_ok("1,3,5-9,15-12");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn parity_on_single_page() {
        // ':odd' on a single page is syntactically valid.
        let pr = parse_ok("5:odd");
        let entries = pr.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::Num(5));
        assert_eq!(entries[0].parity, Some(Parity::Odd));
    }

    // -----------------------------------------------------------------------
    // Invalid inputs
    // -----------------------------------------------------------------------

    #[test]
    fn page_zero_is_invalid() {
        let msg = parse_err("0");
        assert!(msg.contains("0 is invalid"), "got: {msg}");
    }

    #[test]
    fn r0_is_invalid() {
        let msg = parse_err("r0");
        assert!(msg.contains("r0 is invalid"), "got: {msg}");
    }

    #[test]
    fn trailing_dash_is_invalid() {
        let msg = parse_err("1-");
        assert!(msg.contains("expected a page number"), "got: {msg}");
    }

    #[test]
    fn leading_dash_is_invalid() {
        let msg = parse_err("-1");
        assert!(msg.contains("unexpected character"), "got: {msg}");
    }

    #[test]
    fn double_comma_is_invalid() {
        let msg = parse_err("1,,2");
        assert!(
            msg.contains("empty entry") || msg.contains("consecutive commas"),
            "got: {msg}"
        );
    }

    #[test]
    fn trailing_comma_is_invalid() {
        let msg = parse_err("1,2,");
        assert!(msg.contains("trailing comma"), "got: {msg}");
    }

    #[test]
    fn unknown_suffix_is_invalid() {
        let msg = parse_err("1-9:foo");
        assert!(msg.contains("unknown parity suffix"), "got: {msg}");
    }

    #[test]
    fn bare_parity_suffix_selects_nothing_like_qpdf() {
        // qpdf truncates at the suffix (`QUtil.cc:1356`) and then never enters
        // the group loop (`QUtil.cc:1367`), returning an empty result without
        // an error. Probed with qpdf 11.9.0 on a 10-page file:
        // `--pages . :odd --` and `--pages . :even --` both exit 0 and write a
        // 0-page document.
        assert_eq!(resolve(":odd", 10), Vec::<u32>::new());
        assert_eq!(resolve(":even", 10), Vec::<u32>::new());
    }

    #[test]
    fn alpha_input_is_invalid() {
        let msg = parse_err("abc");
        assert!(!msg.is_empty());
    }

    #[test]
    fn lone_r_without_number_is_invalid() {
        let msg = parse_err("r");
        assert!(msg.contains("expected a page number"), "got: {msg}");
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
    fn empty_resolves_to_all() {
        assert_eq!(resolve("", 5), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn empty_constructor_selects_no_pages() {
        // `PageRange::empty()` is the empty source set (qpdf `--from=`), distinct
        // from `parse("")` which is the "all pages" sentinel.
        let none = PageRange::empty();
        assert_eq!(none.entries, Some(Vec::new()));
        assert_eq!(none.resolve(5).unwrap(), Vec::<u32>::new());
        assert_ne!(none, PageRange::parse("").unwrap());
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
        let message = parse_err("x2");
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
