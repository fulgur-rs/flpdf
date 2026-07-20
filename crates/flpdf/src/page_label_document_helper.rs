//! qpdf `QPDFPageLabelDocumentHelper`-equivalent page-label access.
//!
//! [`PageLabelDocumentHelper`] reads, renders (ISO 32000-1 §12.4.2), and edits
//! the catalog `/PageLabels` number tree. [`LabelRange`] models one label range
//! (`/S` style, `/P` prefix, `/St` start). The number-tree walking/building is
//! delegated to [`crate::name_number_tree`].

use crate::name_number_tree::DEFAULT_MAX_TREE_DEPTH;
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

/// Page-label numbering style (ISO 32000-1 §12.4.2 `/S`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    /// `/D` — decimal arabic.
    Decimal,
    /// `/R` — uppercase roman.
    RomanUpper,
    /// `/r` — lowercase roman.
    RomanLower,
    /// `/A` — uppercase letters (A, B, …, Z, AA, …).
    AlphaUpper,
    /// `/a` — lowercase letters.
    AlphaLower,
    /// No `/S` — labels have no numeric portion (prefix only).
    None,
}

impl LabelStyle {
    /// Map a `/S` name's bytes to a style; unrecognised/absent → [`LabelStyle::None`].
    pub fn from_name(name: &[u8]) -> Self {
        match name {
            b"D" => LabelStyle::Decimal,
            b"R" => LabelStyle::RomanUpper,
            b"r" => LabelStyle::RomanLower,
            b"A" => LabelStyle::AlphaUpper,
            b"a" => LabelStyle::AlphaLower,
            _ => LabelStyle::None,
        }
    }

    /// The `/S` name string, or `None` for [`LabelStyle::None`].
    pub fn to_name(self) -> Option<&'static str> {
        match self {
            LabelStyle::Decimal => Some("D"),
            LabelStyle::RomanUpper => Some("R"),
            LabelStyle::RomanLower => Some("r"),
            LabelStyle::AlphaUpper => Some("A"),
            LabelStyle::AlphaLower => Some("a"),
            LabelStyle::None => None,
        }
    }
}

/// One `/PageLabels` range: numbering style, prefix, and starting value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelRange {
    /// Numbering style (`/S`).
    pub style: LabelStyle,
    /// Label prefix (`/P`), decoded; empty when absent.
    pub prefix: String,
    /// First value in the range (`/St`); defaults to 1.
    pub start: i64,
}

impl LabelRange {
    /// Parse a label dictionary (`/S`, `/P`, `/St`). Unrecognised/absent `/S`
    /// → [`LabelStyle::None`]; absent `/St` → 1; `/P` decoded via
    /// `crate::json_inspect::decode_pdf_text_string` with lossy fallback.
    ///
    /// This does **not** resolve indirect `/S`/`/P`/`/St` values (it has no
    /// `Pdf` handle): an indirect inner value falls through to its default.
    /// Callers reading a live document should go through
    /// [`PageLabelDocumentHelper::ranges`] (which uses the resolving
    /// `LabelRange::from_dict_resolved`); this plain form is for the
    /// non-resolving JSON-inspection path.
    pub fn from_dict(dict: &Dictionary) -> Self {
        let style = match dict.get("S") {
            Some(Object::Name(bytes)) => LabelStyle::from_name(bytes),
            _ => LabelStyle::None,
        };
        let prefix = match dict.get("P") {
            Some(Object::String(bytes)) => crate::json_inspect::decode_pdf_text_string(bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(bytes).into_owned()),
            _ => String::new(),
        };
        let start = match dict.get("St") {
            Some(Object::Integer(n)) => *n,
            _ => 1,
        };
        LabelRange {
            style,
            prefix,
            start,
        }
    }

    /// Like [`LabelRange::from_dict`] but resolves indirect `/S`, `/P`, `/St`
    /// values via `pdf` before interpreting them (ISO 32000-1 allows any value
    /// to be an indirect reference). Used by the document reader; the plain
    /// [`LabelRange::from_dict`] is retained for callers without a `Pdf` handle.
    pub(crate) fn from_dict_resolved<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        dict: &Dictionary,
    ) -> Result<Self> {
        let style = match resolve_entry(pdf, dict.get("S"))? {
            Some(Object::Name(bytes)) => LabelStyle::from_name(&bytes),
            _ => LabelStyle::None,
        };
        let prefix = match resolve_entry(pdf, dict.get("P"))? {
            Some(Object::String(bytes)) => crate::json_inspect::decode_pdf_text_string(&bytes)
                .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned()),
            _ => String::new(),
        };
        let start = match resolve_entry(pdf, dict.get("St"))? {
            Some(Object::Integer(n)) => n,
            _ => 1,
        };
        Ok(LabelRange {
            style,
            prefix,
            start,
        })
    }

    /// Build a label dictionary mirroring qpdf `pageLabelDict`: `/S` name when
    /// the style is not [`LabelStyle::None`]; `/P` only when non-empty; `/St`
    /// only when `!= 1`.
    pub fn to_dict(&self) -> Dictionary {
        let mut d = Dictionary::new();
        if let Some(name) = self.style.to_name() {
            d.insert("S", Object::Name(name.into()));
        }
        if !self.prefix.is_empty() {
            d.insert("P", Object::String(self.prefix.clone().into_bytes()));
        }
        if self.start != 1 {
            d.insert("St", Object::Integer(self.start));
        }
        d
    }

    /// Render the display label for `value` (§12.4.2): `prefix` followed by the
    /// style-formatted number. [`LabelStyle::None`] and non-positive numeric
    /// values contribute no numeric portion.
    pub fn format(&self, value: i64) -> String {
        let mut s = self.prefix.clone();
        match self.style {
            LabelStyle::Decimal => s.push_str(&value.to_string()),
            LabelStyle::RomanUpper => s.push_str(&to_roman(value, true)),
            LabelStyle::RomanLower => s.push_str(&to_roman(value, false)),
            LabelStyle::AlphaUpper => s.push_str(&to_alpha(value, true)),
            LabelStyle::AlphaLower => s.push_str(&to_alpha(value, false)),
            LabelStyle::None => {}
        }
        s
    }
}

/// Resolve a dictionary entry that may be an indirect reference, returning the
/// owned target object (or the value verbatim if direct, `None` if absent).
fn resolve_entry<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Option<&Object>,
) -> Result<Option<Object>> {
    match value {
        Some(Object::Reference(r)) => Ok(Some(pdf.resolve_borrowed(*r)?.clone())),
        Some(o) => Ok(Some(o.clone())),
        None => Ok(None),
    }
}

/// Collapse a later `(first_page_idx, LabelRange)` entry into its
/// predecessor when the later entry is redundant — its style, prefix, and
/// `/St` are exactly what the predecessor's own numbering would already
/// produce at that page index. Dropping such an entry does not change any
/// page's rendered label; it only removes a needless explicit tree node.
///
/// `ranges` must be sorted ascending by index (the shape [`PageLabelDocumentHelper::ranges`]
/// and [`PageLabelDocumentHelper::labels_for_page_range`] already produce);
/// only consecutive pairs are compared.
///
/// # Examples
///
/// ```
/// use flpdf::{merge_adjacent_ranges, LabelRange, LabelStyle};
///
/// let a = LabelRange { style: LabelStyle::Decimal, prefix: String::new(), start: 1 };
/// // Index 5 continues `a`'s numbering exactly (1 + 5 == 6): redundant, dropped.
/// let b = LabelRange { start: 6, ..a.clone() };
/// let merged = merge_adjacent_ranges(vec![(0, a), (5, b)]);
/// assert_eq!(merged.len(), 1);
/// ```
pub fn merge_adjacent_ranges(ranges: Vec<(i64, LabelRange)>) -> Vec<(i64, LabelRange)> {
    let mut out: Vec<(i64, LabelRange)> = Vec::with_capacity(ranges.len());
    for (idx, range) in ranges {
        if let Some((prev_idx, prev_range)) = out.last() {
            let expected_start = idx
                .checked_sub(*prev_idx)
                .and_then(|gap| prev_range.start.checked_add(gap));
            if let Some(expected_start) = expected_start {
                if prev_range.style == range.style
                    && prev_range.prefix == range.prefix
                    && range.start == expected_start
                {
                    continue; // redundant with the predecessor — drop the explicit entry
                }
            }
            // Overflow → err on the side of not merging (keep the explicit entry).
        }
        out.push((idx, range));
    }
    out
}

/// Upper bound on the numeric value [`to_roman`]/[`to_alpha`] will render.
///
/// Values above this produce an empty numeric portion — a defensive cap against
/// CPU/memory exhaustion from a hostile `/St`: without it the roman subtraction
/// loop and the alphabetic repeat both scale with `value`, so an `i64::MAX`
/// `/St` would spin/allocate unboundedly. 100 000 is far beyond any real page
/// label yet keeps the rendered string short.
const MAX_RENDERABLE_LABEL_VALUE: i64 = 100_000;

/// Format `value` as a roman numeral (`upper` → uppercase). Empty for
/// `value <= 0` or `value > MAX_RENDERABLE_LABEL_VALUE`.
fn to_roman(value: i64, upper: bool) -> String {
    if value <= 0 || value > MAX_RENDERABLE_LABEL_VALUE {
        return String::new();
    }
    const TABLE: &[(i64, &str, &str)] = &[
        (1000, "M", "m"),
        (900, "CM", "cm"),
        (500, "D", "d"),
        (400, "CD", "cd"),
        (100, "C", "c"),
        (90, "XC", "xc"),
        (50, "L", "l"),
        (40, "XL", "xl"),
        (10, "X", "x"),
        (9, "IX", "ix"),
        (5, "V", "v"),
        (4, "IV", "iv"),
        (1, "I", "i"),
    ];
    let mut v = value;
    let mut out = String::new();
    for &(n, up, lo) in TABLE {
        while v >= n {
            out.push_str(if upper { up } else { lo });
            v -= n;
        }
    }
    out
}

/// Format `value` as repeating letters (§12.4.2): 1→A … 26→Z, 27→AA, 53→AAA.
/// Empty for `value <= 0` or `value > MAX_RENDERABLE_LABEL_VALUE`.
fn to_alpha(value: i64, upper: bool) -> String {
    if value <= 0 || value > MAX_RENDERABLE_LABEL_VALUE {
        return String::new();
    }
    let v = value - 1;
    let letter = (v % 26) as u8;
    let count = (v / 26) + 1;
    let ch = if upper { b'A' + letter } else { b'a' + letter } as char;
    (0..count).map(|_| ch).collect()
}

/// High-level helper for a document's `/PageLabels` number tree.
///
/// Construct with [`PageLabelDocumentHelper::new`] or [`Pdf::page_labels`]. The
/// helper caches nothing; methods re-read the live document.
pub struct PageLabelDocumentHelper<'a, R: Read + Seek> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> PageLabelDocumentHelper<'a, R> {
    /// Create a new helper borrowing `pdf` mutably.
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Resolve the catalog's `/PageLabels` value (Reference or inline dict), or
    /// `None` when absent.
    fn pagelabels_root(&mut self) -> Result<Option<Object>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Some(catalog) = self.pdf.resolve_borrowed(catalog_ref)?.as_dict() else {
            return Ok(None);
        };
        Ok(catalog.get("PageLabels").cloned())
    }

    /// Whether the document carries a `/PageLabels` tree with at least the root.
    ///
    /// # Errors
    ///
    /// - Any error from [`Pdf::resolve`].
    pub fn has_page_labels(&mut self) -> Result<bool> {
        Ok(self.pagelabels_root()?.is_some())
    }

    /// All label ranges as `(first_page_index, LabelRange)`, ascending by index.
    /// Empty when `/PageLabels` is absent.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn ranges(&mut self) -> Result<Vec<(i64, LabelRange)>> {
        let Some(root) = self.pagelabels_root()? else {
            return Ok(vec![]);
        };
        crate::name_number_tree::read_number_tree(
            self.pdf,
            root,
            |pdf, v| {
                let dict = match v {
                    Object::Dictionary(d) => Some(d),
                    Object::Reference(r) => {
                        // A label-range value may be stored behind a holder
                        // chain (ref -> ref -> dict); follow the chain to its
                        // terminal rather than a single hop, then move the
                        // owned dictionary out.
                        let (terminal, _) = resolve_ref_chain(pdf, &Object::Reference(r))?;
                        terminal.into_dict()
                    }
                    _ => None,
                };
                match dict {
                    Some(d) => Ok(Some(LabelRange::from_dict_resolved(pdf, &d)?)),
                    None => Ok(None),
                }
            },
            DEFAULT_MAX_TREE_DEPTH,
        )
    }

    /// The effective label for a 0-based page index (qpdf `getLabelForPage`):
    /// the range whose first index is the largest `<= page_idx`, with `start`
    /// offset to that page. `None` when no range applies (no `/PageLabels`, or
    /// the page precedes the first range).
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn label_for_page(&mut self, page_idx: i64) -> Result<Option<LabelRange>> {
        let ranges = self.ranges()?;
        // ranges is ascending; take the last with first_index <= page_idx.
        let mut chosen: Option<&(i64, LabelRange)> = None;
        for entry in &ranges {
            if entry.0 <= page_idx {
                chosen = Some(entry);
            } else {
                break;
            }
        }
        Ok(chosen.map(|(first, r)| {
            // Saturating arithmetic: `first <= page_idx` so the offset is
            // non-negative, but a hostile `/St` near i64::MAX could otherwise
            // overflow the start (panic in debug, wrap in release).
            let offset = page_idx.saturating_sub(*first);
            LabelRange {
                style: r.style,
                prefix: r.prefix.clone(),
                start: r.start.saturating_add(offset),
            }
        }))
    }

    /// The rendered display string for a 0-based page index. Falls back to
    /// 1-based decimal (`(page_idx + 1)`) when no range applies — matching the
    /// "default 1-based numeric labels" requirement.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn label_string_for_page(&mut self, page_idx: i64) -> Result<String> {
        match self.label_for_page(page_idx)? {
            Some(effective) => Ok(effective.format(effective.start)),
            None => Ok((page_idx + 1).to_string()),
        }
    }

    /// qpdf `getLabelsForPageRange` port: collect the label entries needed to
    /// reproduce the labels of pages `start_idx..=end_idx` if they were
    /// renumbered to begin at `new_start_idx`. Returns `(new_index, LabelRange)`
    /// pairs (the first entry plus every explicit entry in the source range),
    /// renumbered by `new_start_idx - start_idx`. Read-only; intended for
    /// page-extraction wiring (.14.4).
    ///
    /// Unlike qpdf's accumulating signature, this is a single self-contained
    /// call: the leading entry is always emitted (the result vector starts
    /// empty, so there is no prior entry to be redundant against). A later
    /// accumulating consumer can dedupe across calls.
    ///
    /// Re-reads the `/PageLabels` tree once per explicit page in the span (the
    /// helper caches nothing by design); acceptable for typical small label trees.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn labels_for_page_range(
        &mut self,
        start_idx: i64,
        end_idx: i64,
        new_start_idx: i64,
    ) -> Result<Vec<(i64, LabelRange)>> {
        let ranges = self.ranges()?;
        // Set of explicit source indices, for hasIndex().
        let explicit: std::collections::BTreeSet<i64> = ranges.iter().map(|(i, _)| *i).collect();

        // First page label (or fabricated default decimal start = 1 + new_start).
        let first_label = match self.label_for_page(start_idx)? {
            Some(r) => r,
            None => LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: new_start_idx.saturating_add(1),
            },
        };

        let mut out = vec![(new_start_idx, first_label)];
        let idx_offset = new_start_idx.saturating_sub(start_idx);
        // Iterate only the explicit indices within the span (the rest sequence
        // implicitly from the prior entry), so the cost is O(log N + M) in the
        // number of ranges rather than O(end_idx - start_idx) over the page span.
        for &i in explicit.range((start_idx + 1)..=end_idx) {
            if let Some(lab) = self.label_for_page(i)? {
                out.push((i.saturating_add(idx_offset), lab));
            }
        }
        Ok(out)
    }

    /// Collect the raw `(index, value Object)` entries of the `/PageLabels` tree
    /// verbatim (values un-decoded), for rebuild.
    fn raw_entries(&mut self) -> Result<Vec<(i64, Object)>> {
        let Some(root) = self.pagelabels_root()? else {
            return Ok(vec![]);
        };
        crate::name_number_tree::read_number_tree(
            self.pdf,
            root,
            |_, v| Ok(Some(v)),
            DEFAULT_MAX_TREE_DEPTH,
        )
    }

    /// Insert or replace the label range whose first page index is
    /// `first_page_idx`. Rebuilds the `/Nums` tree and points the catalog
    /// `/PageLabels` at the new (indirect) root.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded while reading the existing tree.
    /// - Any error from [`Pdf::resolve`].
    pub fn set_range(&mut self, first_page_idx: i64, range: LabelRange) -> Result<()> {
        let mut entries = self.raw_entries()?;
        let value = Object::Dictionary(range.to_dict());
        match entries.iter_mut().find(|(k, _)| *k == first_page_idx) {
            Some(e) => e.1 = value,
            None => {
                entries.push((first_page_idx, value));
                entries.sort_by_key(|(k, _)| *k);
            }
        }
        self.rebuild(entries)
    }

    /// Remove the label range whose first page index is `first_page_idx`.
    /// Returns `false` if no such range exists. When the last range is removed,
    /// `/PageLabels` is dropped from the catalog.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded while reading the existing tree.
    /// - Any error from [`Pdf::resolve`].
    pub fn remove_range(&mut self, first_page_idx: i64) -> Result<bool> {
        let mut entries = self.raw_entries()?;
        let before = entries.len();
        entries.retain(|(k, _)| *k != first_page_idx);
        if entries.len() == before {
            return Ok(false);
        }
        self.rebuild(entries)?;
        Ok(true)
    }

    /// Replace the entire `/PageLabels` tree with `ranges` — `(first_page_idx,
    /// LabelRange)` pairs, ascending by index (the same shape [`Self::ranges`]
    /// returns). An empty slice removes `/PageLabels` from the catalog
    /// entirely.
    ///
    /// This is the bulk counterpart to [`Self::set_range`]/[`Self::remove_range`]:
    /// where those mutate one entry of the existing tree, `write_labels`
    /// discards whatever the tree currently holds and rebuilds it from the
    /// given list (rebalanced through [`crate::name_number_tree::build_number_tree`],
    /// same leaf-chunking rule as every other tree in this crate).
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] if any range's `/St` (`start`) is
    ///   non-positive, or if any `first_page_idx` is negative — ISO 32000-2
    ///   §7.9.7 defines `/St` as a starting page number (`>= 1`), and a page
    ///   index cannot be negative.
    /// - Any error from [`Pdf::resolve`].
    pub fn write_labels(&mut self, ranges: &[(i64, LabelRange)]) -> Result<()> {
        for (idx, range) in ranges {
            if *idx < 0 {
                return Err(Error::Unsupported(format!(
                    "page label first_page_idx must be >= 0, got {idx}"
                )));
            }
            if range.start < 1 {
                return Err(Error::Unsupported(format!(
                    "page label /St must be >= 1, got {}",
                    range.start
                )));
            }
        }
        let mut entries: Vec<(i64, Object)> = ranges
            .iter()
            .map(|(idx, range)| (*idx, Object::Dictionary(range.to_dict())))
            .collect();
        // build_number_tree requires pre-sorted UNIQUE input; callers
        // (merge_adjacent_ranges, shifted insert/remove lists) already preserve
        // ascending order and normally uniqueness, but this is a public entry
        // point, so sort defensively and dedup by key. ISO 32000-1 §7.9.7
        // requires number-tree keys to be unique; a duplicate would produce
        // a malformed PDF.
        entries.sort_by_key(|(idx, _)| *idx);
        entries.dedup_by(|a, b| a.0 == b.0);
        self.rebuild(entries)
    }

    /// Shift every label range at or after `at` forward by `count`, modeling
    /// `count` pages inserted at 0-based position `at`. Ranges before `at` are
    /// left untouched, so pages inserted in the middle of an existing range's
    /// span inherit that range's numbering (no new explicit entry is needed).
    /// [`merge_adjacent_ranges`] then folds away a shifted range that the
    /// insertion happened to turn into a redundant continuation of its
    /// predecessor (an intentional gap of exactly `count` pages closes up).
    ///
    /// A no-op when `count == 0` or when the document has no `/PageLabels`
    /// (this never fabricates a tree where none existed).
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded while reading the existing tree.
    /// - Any error from [`Pdf::resolve`].
    pub fn insert_pages(&mut self, at: usize, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let ranges = self.ranges()?;
        if ranges.is_empty() {
            return Ok(());
        }
        let at = i64::try_from(at)
            .map_err(|_| Error::Unsupported(format!("insert_pages: at={} exceeds i64::MAX", at)))?;
        let count = i64::try_from(count).map_err(|_| {
            Error::Unsupported(format!("insert_pages: count={} exceeds i64::MAX", count))
        })?;
        let shifted: Vec<(i64, LabelRange)> = ranges
            .into_iter()
            .map(|(idx, range)| {
                if idx >= at {
                    (idx.saturating_add(count), range)
                } else {
                    (idx, range)
                }
            })
            .collect();
        // Shifting can turn a pre-existing intentional jump (e.g. an explicit
        // restart placed exactly `count` pages after its predecessor) into a
        // same-sequence continuation once the gap is filled by the inserted
        // pages; fold it away like `remove_pages` does.
        let merged = merge_adjacent_ranges(shifted);
        self.write_labels(&merged)
    }

    /// Update label ranges for `count` pages removed at 0-based position
    /// `at`, modeling the effect of deleting document pages `at..at+count`.
    ///
    /// Ranges entirely before `at` are kept verbatim. Ranges from `at+count`
    /// onward are recomputed with [`Self::labels_for_page_range`] (the same
    /// renumbering qpdf's `getLabelsForPageRange` performs for page
    /// extraction/merging), so a range whose span is partially consumed by
    /// the removal gets a fresh `/St` reflecting the pages actually lost, and
    /// a range whose entire span falls inside `at..at+count` disappears.
    /// [`merge_adjacent_ranges`] then collapses a trailing entry that turns
    /// out to be redundant with its new predecessor (the common case when the
    /// removed pages sat inside a single, otherwise-uninterrupted range).
    ///
    /// This helper does not know the document's total page count, so removing
    /// pages up to (or past) the end of the labeled range can still produce a
    /// trailing entry describing pages that no longer exist; that entry is
    /// inert (never looked up) but is not pruned here.
    ///
    /// A no-op when `count == 0` or when the document has no `/PageLabels`.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded while reading the existing tree.
    /// - Any error from [`Pdf::resolve`].
    pub fn remove_pages(&mut self, at: usize, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        let ranges = self.ranges()?;
        if ranges.is_empty() {
            return Ok(());
        }
        let at = i64::try_from(at)
            .map_err(|_| Error::Unsupported(format!("remove_pages: at={} exceeds i64::MAX", at)))?;
        let count = i64::try_from(count).map_err(|_| {
            Error::Unsupported(format!("remove_pages: count={} exceeds i64::MAX", count))
        })?;
        let removed_end = at.saturating_add(count);

        // Everything before `at` is unchanged.
        let mut result: Vec<(i64, LabelRange)> = ranges
            .iter()
            .filter(|(idx, _)| *idx < at)
            .cloned()
            .collect();

        // Fabricate the tail's first-label entry from the range effective at
        // `removed_end` (or a LabelStyle::None default if no range applies) —
        // both use `at` as the new base index in the output. This mirrors what
        // the previous `labels_for_page_range` call did, but reuses `ranges`
        // already in scope: O(N) in-memory pass instead of an O(M × N) tree
        // re-parse per surviving explicit index.
        let mut effective_at_removed_end: Option<&(i64, LabelRange)> = None;
        for entry in &ranges {
            if entry.0 <= removed_end {
                effective_at_removed_end = Some(entry);
            } else {
                break;
            }
        }
        let tail_first_label = match effective_at_removed_end {
            Some((first, r)) => {
                let offset = removed_end.saturating_sub(*first);
                LabelRange {
                    style: r.style,
                    prefix: r.prefix.clone(),
                    start: r.start.saturating_add(offset),
                }
            }
            // No explicit range covers `removed_end`: those pages were
            // showing the PDF default label sequence (decimal starting at
            // 1). After removal the page at output index `at` was previously
            // source page `removed_end`, whose default label was
            // `removed_end + 1`; preserve that decimal sequence rather than
            // fabricating a LabelStyle::None entry that would render every
            // surviving page's label as an empty string.
            None => LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: removed_end.saturating_add(1),
            },
        };
        result.push((at, tail_first_label));

        // Every explicit entry past `removed_end` survives, shifted left by
        // `count` so its output index accounts for the removed span.
        let idx_offset = at.saturating_sub(removed_end);
        for (idx, range) in &ranges {
            if *idx > removed_end {
                result.push((idx.saturating_add(idx_offset), range.clone()));
            }
        }

        let merged = merge_adjacent_ranges(result);
        self.write_labels(&merged)
    }

    /// Rebuild `/PageLabels` from sorted entries and patch the catalog. Empty
    /// entries → remove `/PageLabels`.
    fn rebuild(&mut self, entries: Vec<(i64, Object)>) -> Result<()> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(());
        };
        let Some(mut catalog) = self.pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
            return Ok(());
        };

        if entries.is_empty() {
            catalog.remove("PageLabels");
            self.pdf
                .set_object(catalog_ref, Object::Dictionary(catalog));
            return Ok(());
        }

        let mut next_num: u32 = self
            .pdf
            .object_refs()
            .iter()
            .map(|r| r.number)
            .max()
            .unwrap_or(0);
        let mut alloc = move || -> ObjectRef {
            next_num += 1;
            ObjectRef::new(next_num, 0)
        };

        let (root_ref, nodes) = crate::name_number_tree::build_number_tree(&entries, &mut alloc);
        for (r, node) in nodes {
            self.pdf.set_object(r, node);
        }
        catalog.insert("PageLabels", Object::Reference(root_ref));
        self.pdf
            .set_object(catalog_ref, Object::Dictionary(catalog));
        Ok(())
    }
}

/// Extension constructor mirroring [`Pdf::acroform`].
impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level page-label helper for this document.
    pub fn page_labels(&mut self) -> PageLabelDocumentHelper<'_, R> {
        PageLabelDocumentHelper::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A minimal one-page PDF with no `/PageLabels` key at all (as opposed to
    /// [`pdf_with_pagelabels`], whose catalog always carries `/PageLabels`,
    /// even when `/Nums` is empty).
    fn bare_one_page_pdf() -> Pdf<Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.7\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let off2 = bytes.len() as u64;
        bytes.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let off3 = bytes.len() as u64;
        bytes.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(Cursor::new(bytes)).expect("open")
    }

    fn pdf_with_pagelabels(nums: Vec<Object>) -> Pdf<Cursor<Vec<u8>>> {
        // Minimal one-page PDF; then attach an inline /PageLabels leaf via set_object.
        let mut pdf = bare_one_page_pdf();
        // /PageLabels root leaf at obj 10, catalog points to it.
        let pl_ref = ObjectRef::new(10, 0);
        let mut leaf = Dictionary::new();
        leaf.insert("Nums", Object::Array(nums));
        pdf.set_object(pl_ref, Object::Dictionary(leaf));
        let catalog_ref = pdf.root_ref().unwrap();
        let mut catalog = pdf
            .resolve_borrowed(catalog_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        catalog.insert("PageLabels", Object::Reference(pl_ref));
        pdf.set_object(catalog_ref, Object::Dictionary(catalog));
        pdf
    }

    fn label_dict(style: &str, st: Option<i64>, prefix: Option<&str>) -> Object {
        let mut d = Dictionary::new();
        d.insert("S", Object::Name(style.into()));
        if let Some(s) = st {
            d.insert("St", Object::Integer(s));
        }
        if let Some(p) = prefix {
            d.insert("P", Object::String(p.as_bytes().to_vec()));
        }
        Object::Dictionary(d)
    }

    #[test]
    fn label_string_multi_range_matches_spec() {
        // /Nums [0 <</S /r>> 3 <</S /D /St 1>> 6 <</S /D /P "A-" /St 1>>]
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("r", None, None),
            Object::Integer(3),
            label_dict("D", Some(1), None),
            Object::Integer(6),
            label_dict("D", Some(1), Some("A-")),
        ]);
        let mut h = pdf.page_labels();
        assert!(h.has_page_labels().unwrap());
        assert_eq!(h.label_string_for_page(0).unwrap(), "i");
        assert_eq!(h.label_string_for_page(1).unwrap(), "ii");
        assert_eq!(h.label_string_for_page(2).unwrap(), "iii");
        assert_eq!(h.label_string_for_page(3).unwrap(), "1");
        assert_eq!(h.label_string_for_page(5).unwrap(), "3");
        assert_eq!(h.label_string_for_page(6).unwrap(), "A-1");
        assert_eq!(h.label_string_for_page(8).unwrap(), "A-3");
    }

    #[test]
    fn label_for_page_offsets_start() {
        let mut pdf =
            pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(10), None)]);
        let mut h = pdf.page_labels();
        let lab = h.label_for_page(2).unwrap().expect("range applies");
        assert_eq!(lab.start, 12, "/St 10 + offset 2");
        assert_eq!(lab.style, LabelStyle::Decimal);
    }

    #[test]
    fn no_pagelabels_defaults_to_decimal() {
        let mut pdf = pdf_with_pagelabels(vec![]); // empty /Nums -> ranges empty
        let mut h = pdf.page_labels();
        assert_eq!(h.label_string_for_page(0).unwrap(), "1");
        assert_eq!(h.label_string_for_page(4).unwrap(), "5");
        assert!(h.label_for_page(0).unwrap().is_none());
    }

    #[test]
    fn page_before_first_range_defaults_to_decimal() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(3), label_dict("R", Some(1), None)]);
        let mut h = pdf.page_labels();
        assert_eq!(
            h.label_string_for_page(0).unwrap(),
            "1",
            "page before first range"
        );
        assert_eq!(h.label_string_for_page(3).unwrap(), "I");
    }

    #[test]
    fn labels_for_page_range_renumbers_and_copies_explicit() {
        // ranges at 0 (roman) and 5 (decimal). Extract pages 3..=6 to new_start 0.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("r", Some(1), None),
            Object::Integer(5),
            label_dict("D", Some(1), None),
        ]);
        let mut h = pdf.page_labels();
        let out = h.labels_for_page_range(3, 6, 0).unwrap();
        // First page (idx 3) is in the roman range with offset 3 -> start 4.
        assert_eq!(out[0].0, 0);
        assert_eq!(out[0].1.style, LabelStyle::RomanLower);
        assert_eq!(out[0].1.start, 4);
        // Page 5 has an explicit entry -> copied, renumbered to new index 2.
        assert!(out
            .iter()
            .any(|(idx, r)| *idx == 2 && r.style == LabelStyle::Decimal));
    }

    #[test]
    fn set_range_inserts_and_round_trips() {
        let mut pdf = pdf_with_pagelabels(vec![]); // start with empty /PageLabels root
        {
            let mut h = pdf.page_labels();
            h.set_range(
                0,
                LabelRange {
                    style: LabelStyle::RomanLower,
                    prefix: String::new(),
                    start: 1,
                },
            )
            .unwrap();
            h.set_range(
                3,
                LabelRange {
                    style: LabelStyle::Decimal,
                    prefix: "A-".into(),
                    start: 1,
                },
            )
            .unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[0].1.style, LabelStyle::RomanLower);
        assert_eq!(ranges[1].0, 3);
        assert_eq!(ranges[1].1.prefix, "A-");
        assert_eq!(h.label_string_for_page(4).unwrap(), "A-2");
    }

    #[test]
    fn set_range_replaces_existing_index() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.set_range(
                0,
                LabelRange {
                    style: LabelStyle::RomanUpper,
                    prefix: String::new(),
                    start: 1,
                },
            )
            .unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].1.style, LabelStyle::RomanUpper);
    }

    #[test]
    fn remove_range_drops_entry_and_pagelabels_when_empty() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            assert!(h.remove_range(0).unwrap());
            assert!(!h.remove_range(99).unwrap(), "absent index => false");
        }
        let mut h = pdf.page_labels();
        assert!(
            !h.has_page_labels().unwrap(),
            "/PageLabels dropped when empty"
        );
        assert_eq!(
            h.label_string_for_page(0).unwrap(),
            "1",
            "defaults after removal"
        );
    }

    #[test]
    fn roman_matches_spec() {
        assert_eq!(to_roman(1, true), "I");
        assert_eq!(to_roman(4, true), "IV");
        assert_eq!(to_roman(9, false), "ix");
        assert_eq!(to_roman(40, true), "XL");
        assert_eq!(to_roman(90, false), "xc");
        assert_eq!(to_roman(400, true), "CD");
        assert_eq!(to_roman(900, true), "CM");
        assert_eq!(to_roman(3888, true), "MMMDCCCLXXXVIII");
        assert_eq!(to_roman(0, true), "");
        assert_eq!(to_roman(-3, false), "");
    }

    #[test]
    fn formatters_cap_huge_values() {
        // DoS guard: at the cap the formatters still render; above it (incl.
        // i64::MAX) they return empty instead of spinning/allocating unboundedly.
        assert!(!to_roman(MAX_RENDERABLE_LABEL_VALUE, true).is_empty());
        assert_eq!(to_roman(MAX_RENDERABLE_LABEL_VALUE + 1, true), "");
        assert_eq!(to_roman(i64::MAX, true), "");
        assert!(!to_alpha(MAX_RENDERABLE_LABEL_VALUE, true).is_empty());
        assert_eq!(to_alpha(MAX_RENDERABLE_LABEL_VALUE + 1, true), "");
        assert_eq!(to_alpha(i64::MAX, true), "");
    }

    #[test]
    fn alpha_repeating_letters() {
        assert_eq!(to_alpha(1, true), "A");
        assert_eq!(to_alpha(26, true), "Z");
        assert_eq!(to_alpha(27, true), "AA");
        assert_eq!(to_alpha(52, false), "zz");
        assert_eq!(to_alpha(53, true), "AAA");
        assert_eq!(to_alpha(0, true), "");
    }

    #[test]
    fn label_range_format_prefix_and_styles() {
        let d = LabelRange {
            style: LabelStyle::Decimal,
            prefix: "A-".into(),
            start: 1,
        };
        assert_eq!(d.format(5), "A-5");
        let r = LabelRange {
            style: LabelStyle::RomanLower,
            prefix: String::new(),
            start: 1,
        };
        assert_eq!(r.format(3), "iii");
        let none = LabelRange {
            style: LabelStyle::None,
            prefix: "Cover".into(),
            start: 1,
        };
        assert_eq!(
            none.format(9),
            "Cover",
            "None style => prefix only, no number"
        );
    }

    #[test]
    fn label_range_dict_round_trip() {
        let r = LabelRange {
            style: LabelStyle::RomanUpper,
            prefix: "App-".into(),
            start: 5,
        };
        let dict = r.to_dict();
        assert_eq!(dict.get("S"), Some(&Object::Name("R".into())));
        assert_eq!(dict.get("St"), Some(&Object::Integer(5)));
        assert_eq!(LabelRange::from_dict(&dict), r);
        // Defaults omitted: St=1 and empty prefix and None style produce empty dict.
        let bare = LabelRange {
            style: LabelStyle::None,
            prefix: String::new(),
            start: 1,
        };
        assert!(
            bare.to_dict().iter().next().is_none(),
            "all-default range => empty dict"
        );
    }

    #[test]
    fn ranges_resolves_indirect_inner_st() {
        let mut pdf = pdf_with_pagelabels(vec![]); // empty root; we set a custom tree below
                                                   // Put an indirect /St value: label dict {/S /D /St 11 0 R}, 11 0 obj = Integer(7).
        let st_ref = ObjectRef::new(11, 0);
        pdf.set_object(st_ref, Object::Integer(7));
        let mut label = Dictionary::new();
        label.insert("S", Object::Name("D".into()));
        label.insert("St", Object::Reference(st_ref));
        let pl_ref = ObjectRef::new(10, 0);
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![Object::Integer(0), Object::Dictionary(label)]),
        );
        pdf.set_object(pl_ref, Object::Dictionary(leaf));
        // catalog already points /PageLabels -> 10 0 R from pdf_with_pagelabels.
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].1.start, 7, "indirect /St must be resolved");
    }

    #[test]
    fn set_range_round_trips_multi_leaf_tree() {
        let mut pdf = pdf_with_pagelabels(vec![]);
        {
            let mut h = pdf.page_labels();
            for i in 0..40i64 {
                h.set_range(
                    i * 2,
                    LabelRange {
                        style: LabelStyle::Decimal,
                        prefix: String::new(),
                        start: 1,
                    },
                )
                .unwrap();
            }
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(
            ranges.len(),
            40,
            "all 40 ranges survive the multi-leaf tree"
        );
        // Spot-check ordering + a mid entry.
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[39].0, 78);
        assert!(ranges.windows(2).all(|w| w[0].0 < w[1].0), "ascending");
    }

    #[test]
    fn label_style_name_round_trip_all_variants() {
        for (bytes, style, name) in [
            (b"D".as_ref(), LabelStyle::Decimal, Some("D")),
            (b"R".as_ref(), LabelStyle::RomanUpper, Some("R")),
            (b"r".as_ref(), LabelStyle::RomanLower, Some("r")),
            (b"A".as_ref(), LabelStyle::AlphaUpper, Some("A")),
            (b"a".as_ref(), LabelStyle::AlphaLower, Some("a")),
        ] {
            assert_eq!(LabelStyle::from_name(bytes), style);
            assert_eq!(style.to_name(), name);
        }
        // Unrecognised /S name -> None (from_name `_` arm); None has no name.
        assert_eq!(LabelStyle::from_name(b"Z"), LabelStyle::None);
        assert_eq!(LabelStyle::None.to_name(), None);
    }

    #[test]
    fn format_alpha_styles() {
        let up = LabelRange {
            style: LabelStyle::AlphaUpper,
            prefix: String::new(),
            start: 1,
        };
        assert_eq!(up.format(27), "AA");
        let lo = LabelRange {
            style: LabelStyle::AlphaLower,
            prefix: "x".into(),
            start: 1,
        };
        assert_eq!(lo.format(2), "xb");
    }

    #[test]
    fn from_dict_non_name_style_is_none() {
        let mut d = Dictionary::new();
        d.insert("S", Object::Integer(0)); // /S not a Name -> LabelStyle::None
        assert_eq!(LabelRange::from_dict(&d).style, LabelStyle::None);
    }

    #[test]
    fn ranges_handles_indirect_and_non_dict_values() {
        // entry 0: indirect ref to a label dict; entry 5: a non-dict value (skipped).
        let mut pdf = pdf_with_pagelabels(vec![]);
        let lab_ref = ObjectRef::new(20, 0);
        let mut lab = Dictionary::new();
        lab.insert("S", Object::Name("D".into()));
        pdf.set_object(lab_ref, Object::Dictionary(lab));
        let pl_ref = ObjectRef::new(10, 0);
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![
                Object::Integer(0),
                Object::Reference(lab_ref), // indirect entry value -> resolve
                Object::Integer(5),
                Object::Integer(99), // non-dict entry value -> skipped
            ]),
        );
        pdf.set_object(pl_ref, Object::Dictionary(leaf));
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1, "non-dict value skipped");
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[0].1.style, LabelStyle::Decimal);
    }

    #[test]
    fn ranges_non_name_style_resolves_to_none() {
        // A label dict whose /S is not a Name resolves to LabelStyle::None via
        // the resolving reader path (from_dict_resolved).
        let mut pdf = pdf_with_pagelabels(vec![]);
        let pl_ref = ObjectRef::new(10, 0);
        let mut lab = Dictionary::new();
        lab.insert("S", Object::Integer(0)); // non-Name /S
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![Object::Integer(0), Object::Dictionary(lab)]),
        );
        pdf.set_object(pl_ref, Object::Dictionary(leaf));
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].1.style, LabelStyle::None);
    }

    #[test]
    fn labels_for_page_range_fabricates_default_when_first_unlabeled() {
        // Only an explicit range at index 5; extract starting before it (idx 0).
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(5), label_dict("D", Some(1), None)]);
        let mut h = pdf.page_labels();
        let out = h.labels_for_page_range(0, 6, 0).unwrap();
        // First entry fabricated: Decimal, start = new_start(0) + 1 = 1.
        assert_eq!(out[0].0, 0);
        assert_eq!(out[0].1.style, LabelStyle::Decimal);
        assert_eq!(out[0].1.start, 1);
        // The explicit range at 5 is copied (renumbered to 5).
        assert!(out.iter().any(|(idx, _)| *idx == 5));
    }

    #[test]
    fn helper_tolerates_non_dict_catalog() {
        let mut pdf = pdf_with_pagelabels(vec![]);
        let catalog_ref = pdf.root_ref().unwrap();
        pdf.set_object(catalog_ref, Object::Integer(0)); // catalog no longer a dict
        let mut h = pdf.page_labels();
        assert!(
            !h.has_page_labels().unwrap(),
            "non-dict catalog => no labels"
        );
        assert_eq!(h.ranges().unwrap(), vec![]);
        // rebuild path bails out gracefully when the catalog is not a dict.
        h.set_range(
            0,
            LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn helper_tolerates_missing_root() {
        // A trailer without /Root makes root_ref() return None; the helper must
        // degrade gracefully (no labels, rebuild is a no-op).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.7\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("rootless trailer still opens");
        assert!(pdf.root_ref().is_none(), "rootless trailer => no root_ref");
        let mut h = pdf.page_labels();
        assert!(!h.has_page_labels().unwrap());
        assert_eq!(h.ranges().unwrap(), vec![]);
        h.set_range(
            0,
            LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: 1,
            },
        )
        .unwrap();
    }

    /// Shorthand for a plain decimal range starting at `start`, no prefix.
    fn dec(start: i64) -> LabelRange {
        LabelRange {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            start,
        }
    }

    // ── write_labels ──────────────────────────────────────────────────────

    #[test]
    fn write_labels_round_trips_multi_style_ranges() {
        let mut pdf = pdf_with_pagelabels(vec![]); // start with no /PageLabels
        let ranges = vec![
            (
                0,
                LabelRange {
                    style: LabelStyle::RomanLower,
                    prefix: String::new(),
                    start: 1,
                },
            ),
            (
                3,
                LabelRange {
                    style: LabelStyle::Decimal,
                    prefix: "A-".into(),
                    start: 1,
                },
            ),
            (
                7,
                LabelRange {
                    style: LabelStyle::AlphaUpper,
                    prefix: String::new(),
                    start: 1,
                },
            ),
        ];
        {
            let mut h = pdf.page_labels();
            h.write_labels(&ranges).unwrap();
        }
        let mut h = pdf.page_labels();
        assert_eq!(h.ranges().unwrap(), ranges);
    }

    #[test]
    fn write_labels_empty_removes_pagelabels() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.write_labels(&[]).unwrap();
        }
        let mut h = pdf.page_labels();
        assert!(
            !h.has_page_labels().unwrap(),
            "/PageLabels removed by an empty write_labels"
        );
    }

    #[test]
    fn write_labels_rejects_negative_start() {
        let mut pdf = bare_one_page_pdf();
        let mut h = pdf.page_labels();
        let err = h
            .write_labels(&[(0, dec(-1))])
            .expect_err("/St < 1 must be rejected");
        assert!(matches!(err, Error::Unsupported(_)));
        assert!(
            !h.has_page_labels().unwrap(),
            "rejected write must not partially apply"
        );
    }

    #[test]
    fn write_labels_rejects_negative_index() {
        let mut pdf = bare_one_page_pdf();
        let mut h = pdf.page_labels();
        let err = h
            .write_labels(&[(-1, dec(1))])
            .expect_err("negative first_page_idx must be rejected");
        assert!(matches!(err, Error::Unsupported(_)));
    }

    // ── insert_pages ──────────────────────────────────────────────────────

    #[test]
    fn insert_pages_in_middle_shifts_only_ranges_at_or_after_it() {
        // Roman range at 0, decimal range at 5. Insert 2 pages at position 3,
        // inside the roman range's span.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("r", Some(1), None),
            Object::Integer(5),
            label_dict("D", Some(1), None),
        ]);
        {
            let mut h = pdf.page_labels();
            h.insert_pages(3, 2).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges[0].0, 0, "range before the insertion point stays put");
        assert_eq!(
            ranges[1].0, 7,
            "range at/after the insertion point shifts by count"
        );
    }

    #[test]
    fn insert_pages_at_beginning_shifts_first_range_and_leading_pages_default() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.insert_pages(0, 2).unwrap();
        }
        let mut h = pdf.page_labels();
        assert_eq!(h.ranges().unwrap(), vec![(2, dec(1))]);
        // The two newly-inserted leading pages precede any range, so they fall
        // back to the plain 1-based default rather than inheriting page 2's "1".
        assert_eq!(h.label_string_for_page(0).unwrap(), "1");
        assert_eq!(h.label_string_for_page(2).unwrap(), "1");
    }

    #[test]
    fn insert_pages_merges_shift_that_closes_an_exact_gap() {
        // (5, Decimal, start 8) is an intentional forward jump over (0,
        // Decimal, start 1) -- numbers 6 and 7 are deliberately skipped.
        // Inserting exactly 2 pages at position 2 shifts it to index 7, and
        // 1 + 7 == 8: the gap the insertion fills is exactly the jump that
        // made the second entry non-redundant, so it must now collapse.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("D", Some(1), None),
            Object::Integer(5),
            label_dict("D", Some(8), None),
        ]);
        {
            let mut h = pdf.page_labels();
            h.insert_pages(2, 2).unwrap();
        }
        let mut h = pdf.page_labels();
        assert_eq!(h.ranges().unwrap(), vec![(0, dec(1))]);
    }

    #[test]
    fn insert_pages_after_last_range_leaves_entries_unchanged() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.insert_pages(10, 3).unwrap(); // append pages well past the only range
        }
        let mut h = pdf.page_labels();
        assert_eq!(h.ranges().unwrap(), vec![(0, dec(1))]);
    }

    #[test]
    fn insert_pages_noop_on_empty_tree() {
        let mut pdf = bare_one_page_pdf();
        let mut h = pdf.page_labels();
        h.insert_pages(0, 5).unwrap();
        assert!(
            !h.has_page_labels().unwrap(),
            "insert_pages must not fabricate a tree where none existed"
        );
    }

    #[test]
    fn insert_pages_noop_when_count_zero() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.insert_pages(3, 0).unwrap();
        }
        let mut h = pdf.page_labels();
        assert_eq!(h.ranges().unwrap(), vec![(0, dec(1))]);
    }

    // ── remove_pages ──────────────────────────────────────────────────────

    #[test]
    fn remove_pages_partial_delete_leaves_gap_entry() {
        // A single range at 0 covers the whole document. Deleting page index 2
        // means the numbers that belonged to it are gone, so the surviving
        // pages after it need a fresh explicit entry (no silent renumbering).
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.remove_pages(2, 1).unwrap();
        }
        let mut h = pdf.page_labels();
        assert_eq!(h.ranges().unwrap(), vec![(0, dec(1)), (2, dec(4))]);
    }

    #[test]
    fn remove_pages_wipes_range_entirely_consumed_by_removal() {
        // Decimal at 0, roman spanning indices 5..8, alpha at 8. Remove exactly
        // the roman range's span.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("D", Some(1), None),
            Object::Integer(5),
            label_dict("R", Some(1), None),
            Object::Integer(8),
            label_dict("A", Some(1), None),
        ]);
        {
            let mut h = pdf.page_labels();
            h.remove_pages(5, 3).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(
            ranges,
            vec![
                (0, dec(1)),
                (
                    5,
                    LabelRange {
                        style: LabelStyle::AlphaUpper,
                        prefix: String::new(),
                        start: 1
                    }
                ),
            ]
        );
        assert!(
            !ranges
                .iter()
                .any(|(_, r)| r.style == LabelStyle::RomanUpper),
            "the roman range is fully consumed by the removal"
        );
    }

    #[test]
    fn remove_pages_spanning_multiple_ranges_consumes_middle_range() {
        // Decimal at 0, roman spanning 3..7, alpha at 7. Remove indices 2..8:
        // the tail of decimal, all of roman, and the head of alpha.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("D", Some(1), None),
            Object::Integer(3),
            label_dict("R", Some(1), None),
            Object::Integer(7),
            label_dict("A", Some(1), None),
        ]);
        {
            let mut h = pdf.page_labels();
            h.remove_pages(2, 6).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(
            ranges,
            vec![
                (0, dec(1)),
                (
                    2,
                    LabelRange {
                        style: LabelStyle::AlphaUpper,
                        prefix: String::new(),
                        start: 2
                    }
                ),
            ]
        );
    }

    #[test]
    fn remove_pages_collapses_pre_existing_redundant_neighbor() {
        // (5, Decimal, start 6) is already exactly the natural continuation of
        // (0, Decimal, start 1) (1 + (5-0) == 6); this pair survives untouched
        // in the head, and write_labels re-merges it via merge_adjacent_ranges
        // on every rebuild. Removing pages far past both (20..23) exercises the
        // real-gap tail entry at the same time: it must NOT merge with (0,1)
        // once the redundant (5,6) is folded away, because 3 pages of real
        // numbering were actually consumed by the removal.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("D", Some(1), None),
            Object::Integer(5),
            label_dict("D", Some(6), None),
        ]);
        {
            let mut h = pdf.page_labels();
            h.remove_pages(20, 3).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges, vec![(0, dec(1)), (20, dec(24))]);
    }

    #[test]
    fn remove_pages_noop_on_empty_tree() {
        let mut pdf = bare_one_page_pdf();
        let mut h = pdf.page_labels();
        h.remove_pages(0, 3).unwrap();
        assert!(
            !h.has_page_labels().unwrap(),
            "remove_pages must not fabricate a tree where none existed"
        );
    }

    #[test]
    fn remove_pages_noop_when_count_zero() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.remove_pages(0, 0).unwrap();
        }
        let mut h = pdf.page_labels();
        assert_eq!(h.ranges().unwrap(), vec![(0, dec(1))]);
    }

    /// Covers the `None` arm of the `effective_at_removed_end` match in
    /// remove_pages: when `removed_end` is BEFORE the first explicit range,
    /// the surviving pages must keep the PDF-default decimal label sequence
    /// they had before removal (starting at `removed_end + 1`), NOT get a
    /// LabelStyle::None entry that would render every label as an empty
    /// string.
    #[test]
    fn remove_pages_before_first_range_preserves_default_decimal_sequence() {
        // Ranges start at index 5 (roman), leaving 0..5 with the PDF
        // default label sequence "1"…"5".
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(5), label_dict("r", Some(1), None)]);
        // Remove pages 0..2 — removed_end=2, before the first range at index 5.
        // Old source page 2 (previously "3") now becomes output page 0.
        {
            let mut h = pdf.page_labels();
            h.remove_pages(0, 2).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        // Two entries survive: an explicit decimal range starting at 3 (so
        // new page 0 renders as "3", matching source page 2), and the
        // original roman range now at index 3 (5 - 2 shift).
        assert_eq!(ranges.len(), 2, "got {ranges:?}");
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[0].1.style, LabelStyle::Decimal);
        assert_eq!(ranges[0].1.start, 3);
        assert_eq!(ranges[1].0, 3);
        assert_eq!(ranges[1].1.style, LabelStyle::RomanLower);
        // End-to-end: the rendered label for new page 0 must be "3".
        assert_eq!(h.label_string_for_page(0).unwrap(), "3");
    }

    /// Cover the trailing shift-loop `if *idx > removed_end` in remove_pages:
    /// deletion touches only the first range, so a downstream range must
    /// survive with its output index shifted left.
    #[test]
    fn remove_pages_shifts_trailing_range_past_removed_span() {
        // Two ranges: roman starting at 0, decimal restart at 4. Remove
        // one page at index 0, so the trailing range must shift to index 3.
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("r", Some(1), None),
            Object::Integer(4),
            label_dict("D", Some(1), None),
        ]);
        {
            let mut h = pdf.page_labels();
            h.remove_pages(0, 1).unwrap();
        }
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        // Trailing decimal range slides from index 4 to 3.
        assert!(
            ranges
                .iter()
                .any(|(idx, r)| *idx == 3 && r.style == LabelStyle::Decimal),
            "trailing range must survive at shifted index 3: {ranges:?}"
        );
    }

    // ── merge_adjacent_ranges ─────────────────────────────────────────────

    #[test]
    fn merge_adjacent_ranges_collapses_contiguous_identical_neighbor() {
        // 1 + (5-0) == 6: the second entry adds no information.
        let merged = merge_adjacent_ranges(vec![(0, dec(1)), (5, dec(6))]);
        assert_eq!(merged, vec![(0, dec(1))]);
    }

    #[test]
    fn merge_adjacent_ranges_keeps_non_contiguous_start() {
        let ranges = vec![(0, dec(1)), (5, dec(100))];
        assert_eq!(merge_adjacent_ranges(ranges.clone()), ranges);
    }

    #[test]
    fn merge_adjacent_ranges_keeps_style_mismatch() {
        let b = LabelRange {
            style: LabelStyle::RomanUpper,
            prefix: String::new(),
            start: 6, // numerically contiguous with dec(1), but a different style
        };
        let ranges = vec![(0, dec(1)), (5, b)];
        assert_eq!(
            merge_adjacent_ranges(ranges.clone()),
            ranges,
            "different style must block the merge even when /St lines up"
        );
    }

    #[test]
    fn merge_adjacent_ranges_keeps_prefix_mismatch() {
        let a = LabelRange {
            style: LabelStyle::Decimal,
            prefix: "A-".into(),
            start: 1,
        };
        let b = LabelRange {
            style: LabelStyle::Decimal,
            prefix: "B-".into(),
            start: 6,
        };
        let ranges = vec![(0, a), (5, b)];
        assert_eq!(
            merge_adjacent_ranges(ranges.clone()),
            ranges,
            "different prefix must block the merge even when style/St line up"
        );
    }

    #[test]
    fn merge_adjacent_ranges_handles_empty_and_singleton() {
        assert_eq!(merge_adjacent_ranges(vec![]), vec![]);
        let only = vec![(0, dec(1))];
        assert_eq!(merge_adjacent_ranges(only.clone()), only);
    }

    #[test]
    fn merge_adjacent_ranges_skips_merge_on_arithmetic_overflow() {
        // Unsorted input (idx < prev_idx) → checked_sub underflows → no merge.
        // The function is total: it must not panic and must preserve the entry.
        let a = LabelRange {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            start: 1,
        };
        let b = a.clone();
        let unsorted = vec![(10, a), (5, b)];
        assert_eq!(
            merge_adjacent_ranges(unsorted.clone()),
            unsorted,
            "underflow in gap arithmetic must fall through, not merge"
        );

        // Add-overflow branch: prev.start = i64::MAX with a positive gap
        // saturates in the old code; checked_add now short-circuits.
        let big = LabelRange {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            start: i64::MAX,
        };
        let follow = LabelRange {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            start: 0, // any value; the point is that checked_add must be None
        };
        let overflow = vec![(0, big), (1, follow)];
        assert_eq!(
            merge_adjacent_ranges(overflow.clone()),
            overflow,
            "add overflow must fall through, not merge"
        );
    }

    #[test]
    fn insert_pages_rejects_at_or_count_exceeding_i64_max() {
        // Need a document with at least one range so we get past the
        // early-return; usize::MAX > i64::MAX on any target with usize >= 64-bit.
        // (On 32-bit targets usize::MAX < i64::MAX and try_from succeeds; those
        // are not our supported targets for this behaviour.)
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            Object::Dictionary(Dictionary::new()),
        ]);
        let mut helper = PageLabelDocumentHelper::new(&mut pdf);
        let err = helper.insert_pages(usize::MAX, 1).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
        let err = helper.insert_pages(0, usize::MAX).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }

    #[test]
    fn remove_pages_rejects_at_or_count_exceeding_i64_max() {
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            Object::Dictionary(Dictionary::new()),
        ]);
        let mut helper = PageLabelDocumentHelper::new(&mut pdf);
        let err = helper.remove_pages(usize::MAX, 1).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
        let err = helper.remove_pages(0, usize::MAX).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
    }
}
