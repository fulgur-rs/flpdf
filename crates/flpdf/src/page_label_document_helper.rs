//! qpdf correspondence: `QPDFPageLabelDocumentHelper.cc` canonical page-label access and reconstruction.
//!
//! [`PageLabelDocumentHelper`] reads, reconstructs, and renders (ISO 32000-1
//! §12.4.2) the catalog `/PageLabels` number tree. The qpdf-shaped read
//! methods retain live [`ObjectHandle`] values for raw `/S`, `/P`, and `/St`
//! semantics; [`LabelRange`] is the typed view used by reconstruction and
//! display APIs.

use crate::nntree::DEFAULT_MAX_TREE_DEPTH;
use crate::{Error, ObjectHandle, Pdf, Result};
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
    /// Decode a live qpdf-shaped label dictionary without materializing it as
    /// a legacy [`crate::Object`]. Unknown `/S` names remain unknown to the raw
    /// handle, while this typed compatibility view retains the historical
    /// `LabelStyle::None` mapping.
    fn from_handle<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        handle: &ObjectHandle,
    ) -> Result<Option<Self>> {
        let handle = pdf.resolve_to_terminal(handle)?;
        if handle.try_as_dictionary()?.is_none() {
            return Ok(None);
        }
        let style = pdf
            .resolve_to_terminal(&handle.try_get_key(b"/S")?)?
            .try_as_name()?
            .map(|name| LabelStyle::from_name(&name))
            .unwrap_or(LabelStyle::None);
        let prefix = pdf
            .resolve_to_terminal(&handle.try_get_key(b"/P")?)?
            .as_string()
            .map(|bytes| {
                crate::json_inspect::decode_pdf_text_string(&bytes)
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned())
            })
            .unwrap_or_default();
        let start = handle.try_get_key(b"/St")?.try_as_integer()?.unwrap_or(1);
        Ok(Some(Self {
            style,
            prefix,
            start,
        }))
    }

    /// Render the display label for `value` (§12.4.2): `prefix` followed by the
    /// style-formatted number. [`LabelStyle::None`] and non-positive numeric
    /// values contribute no numeric portion.
    // qpdf-deviation-start: page-label rendering to a display string
    // (roman/alpha/decimal, ISO 32000-1 §12.4.2) has no qpdf counterpart --
    // QPDFPageLabelDocumentHelper.cc only produces/consumes the raw
    // /S,/P,/St dictionary and qpdf never computes a rendered numeral
    // string anywhere in its source (doJSONPageLabels emits the raw dict
    // via getJSON, never a rendered label).
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
    // qpdf-deviation-end
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
            // Overflow signals either an unsorted input (checked_sub
            // underflow) or a pathological i64::MAX-adjacent start (checked_add
            // overflow); adversarial input or a caller bug. Safety first —
            // keep the explicit entry rather than trust a synthetic
            // "expected_start" that could accidentally match. Redundant
            // entries never break correctness, only compactness.
        }
        out.push((idx, range));
    }
    out
}

/// qpdf's page-selection fold, retaining the raw presence of `/P` in each
/// reconstructed label dictionary. An explicitly empty `/P` is not redundant
/// with an absent `/P`: qpdf compares the raw handles while deciding whether
/// to skip a subsequent entry (`QPDFPageLabelDocumentHelper.cc:57-79`).
pub fn merge_adjacent_ranges_with_prefix_presence(
    ranges: Vec<(i64, LabelRange, bool)>,
) -> Vec<(i64, LabelRange, bool)> {
    let mut out: Vec<(i64, LabelRange, bool)> = Vec::with_capacity(ranges.len());
    for (idx, range, prefix_present) in ranges {
        if let Some((prev_idx, prev_range, prev_prefix_present)) = out.last() {
            let expected_start = idx
                .checked_sub(*prev_idx)
                .and_then(|gap| prev_range.start.checked_add(gap));
            if let Some(expected_start) = expected_start {
                if prev_range.style == range.style
                    && prev_range.prefix == range.prefix
                    && *prev_prefix_present == prefix_present
                    && range.start == expected_start
                {
                    continue;
                }
            }
        }
        out.push((idx, range, prefix_present));
    }
    out
}

// qpdf-deviation-start: page-label rendering (this const and the two
// functions below) has no qpdf counterpart -- see the marker on
// LabelRange::format for the full citation.
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
// qpdf-deviation-end

/// High-level helper for a document's `/PageLabels` number tree.
///
/// Construct with [`PageLabelDocumentHelper::new`] or [`Pdf::page_labels`]. The
/// helper caches nothing; methods re-read the live document.
pub struct PageLabelDocumentHelper<'a, R: Read + Seek + 'static> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> PageLabelDocumentHelper<'a, R> {
    /// Create a new helper borrowing `pdf` mutably.
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Return the live catalog `/PageLabels` value from the canonical handle
    /// graph. Values that resolve to null are absent, matching qpdf's
    /// `QPDF_Dictionary::hasKey` visibility rule.
    fn pagelabels_root_handle(&mut self) -> Result<Option<ObjectHandle>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let catalog = self.pdf.get_object_handle(catalog_ref);
        let catalog = self.pdf.resolve_to_terminal(&catalog)?;
        if catalog.try_as_dictionary()?.is_none() {
            return Ok(None);
        }
        // qpdf's QPDF_Dictionary::hasKey hides values that are null, including
        // an indirect reference that resolves to null (QPDF_Dictionary.cc:98-101).
        if !catalog.try_has_key(b"/PageLabels")? {
            return Ok(None);
        }
        Ok(Some(catalog.try_get_key(b"/PageLabels")?))
    }

    fn pagelabels_tree(&mut self) -> Result<Option<crate::nntree::NumberTree>> {
        let Some(root) = self.pagelabels_root_handle()? else {
            return Ok(None);
        };
        let mut tree = crate::nntree::NumberTree::new(root, true);
        tree.set_max_depth(DEFAULT_MAX_TREE_DEPTH);
        Ok(Some(tree))
    }

    /// Whether the document carries a `/PageLabels` tree with at least the root.
    ///
    /// # Errors
    ///
    /// - Any error from [`Pdf::resolve`].
    pub fn has_page_labels(&mut self) -> Result<bool> {
        Ok(self.pagelabels_root_handle()?.is_some())
    }

    /// Build qpdf's direct label dictionary for a numbering style, starting
    /// value, and optional prefix (`pageLabelDict`).
    pub fn page_label_dict(style: LabelStyle, start_num: i64, prefix: &str) -> ObjectHandle {
        let result = ObjectHandle::dictionary(Vec::new());
        if let Some(name) = style.to_name() {
            result
                .replace_key(b"/S", ObjectHandle::name(name.as_bytes().to_vec()))
                .expect("new direct page-label dictionary is unowned");
        }
        if !prefix.is_empty() {
            let bytes = crate::pdf_string::new_unicode_string(prefix.as_bytes());
            result
                .replace_key(b"/P", ObjectHandle::string(bytes))
                .expect("new direct page-label dictionary is unowned");
        }
        if start_num != 1 {
            result
                .replace_key(b"/St", ObjectHandle::integer(start_num))
                .expect("new direct page-label dictionary is unowned");
        }
        result
    }

    fn reconstructed_label_handle(
        range: &LabelRange,
        prefix_present: bool,
    ) -> Result<ObjectHandle> {
        let result = Self::page_label_dict(range.style, range.start, &range.prefix);
        if prefix_present && range.prefix.is_empty() {
            result.replace_key(b"/P", ObjectHandle::string(Vec::new()))?;
        }
        result.replace_key(b"/St", ObjectHandle::integer(range.start))?;
        Ok(result)
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
        let Some(mut tree) = self.pagelabels_tree()? else {
            return Ok(vec![]);
        };
        let raw_entries = tree.as_map(self.pdf)?;
        let mut entries = Vec::with_capacity(raw_entries.len());
        for (index, value) in raw_entries {
            if let Some(range) = LabelRange::from_handle(self.pdf, &value)? {
                entries.push((index, range));
            }
        }
        Ok(entries)
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
        let Some(label) = self.get_label_for_page(page_idx)? else {
            return Ok(None);
        };
        LabelRange::from_handle(self.pdf, &label)
    }

    /// Return qpdf's raw reconstructed label dictionary for a 0-based page
    /// index. The returned direct dictionary always contains `/St`; `/S` and
    /// `/P` retain the source handles' exact presence, absence, and values.
    ///
    /// This is qpdf `getLabelForPage`: an unknown `/S` name and an explicit
    /// empty `/P ()` are preserved rather than normalized into [`LabelRange`].
    ///
    /// # Errors
    ///
    /// Returns errors from canonical object resolution, number-tree traversal,
    /// or checked `/St` offset arithmetic.
    pub fn get_label_for_page(&mut self, page_idx: i64) -> Result<Option<ObjectHandle>> {
        let Some(mut tree) = self.pagelabels_tree()? else {
            return Ok(None);
        };
        self.get_label_for_page_from_tree(&mut tree, page_idx)
    }

    /// Append qpdf's reconstructed label entries for an inclusive source page
    /// range to `labels`.
    ///
    /// Each tuple is `(new_page_index, raw_label_dictionary)`. The first entry
    /// is fabricated with `/St = new_start_idx + 1` when no effective source
    /// label exists. Existing trailing entries are checked using qpdf's raw
    /// `/S`/`/P`/`/St` redundancy rule before the first entry is appended.
    ///
    /// This is qpdf `getLabelsForPageRange`; callers may invoke it repeatedly
    /// for multiple input documents and preserve the accumulated vector.
    ///
    /// # Errors
    ///
    /// Returns errors from canonical object resolution, number-tree traversal,
    /// or checked index arithmetic.
    pub fn get_labels_for_page_range(
        &mut self,
        start_idx: i64,
        end_idx: i64,
        new_start_idx: i64,
        labels: &mut Vec<(i64, ObjectHandle)>,
    ) -> Result<()> {
        let idx_offset = new_start_idx
            .checked_sub(start_idx)
            .ok_or_else(|| Error::Unsupported("page label index offset overflow".to_string()))?;
        let mut tree = self.pagelabels_tree()?;
        let first_label = match tree.as_mut() {
            Some(tree) => self
                .get_label_for_page_from_tree(tree, start_idx)?
                .unwrap_or_else(|| ObjectHandle::dictionary(Vec::new())),
            None => ObjectHandle::dictionary(Vec::new()),
        };
        if !first_label.try_has_key(b"/St")? {
            let default_start = new_start_idx.checked_add(1).ok_or_else(|| {
                Error::Unsupported("page label fabricated start overflow".to_string())
            })?;
            first_label.replace_key(b"/St", ObjectHandle::integer(default_start))?;
        }

        let skip_first = if let Some((last_index, last_label)) = labels.last() {
            if last_label.try_as_dictionary()?.is_some()
                && first_label.try_as_dictionary()?.is_some()
            {
                let last_s = last_label.try_get_key(b"/S")?;
                let first_s = first_label.try_get_key(b"/S")?;
                let last_p = last_label.try_get_key(b"/P")?;
                let first_p = first_label.try_get_key(b"/P")?;
                let last_st = last_label.try_get_key(b"/St")?.try_as_integer()?;
                let first_st = first_label.try_get_key(b"/St")?.try_as_integer()?;
                let idx_delta = new_start_idx.checked_sub(*last_index);
                let st_delta = first_st
                    .and_then(|first_st| last_st.and_then(|last_st| first_st.checked_sub(last_st)));
                idx_delta.zip(st_delta).is_some_and(|(idx, st)| {
                    idx == st
                        && last_s.unparse() == first_s.unparse()
                        && last_p.unparse() == first_p.unparse()
                })
            } else {
                false
            }
        } else {
            false
        };
        if !skip_first {
            labels.push((new_start_idx, first_label));
        }

        if let Some(tree) = tree.as_mut() {
            let mut source_idx = start_idx;
            while source_idx < end_idx {
                source_idx = source_idx.checked_add(1).ok_or_else(|| {
                    // cov:ignore-start: source_idx < end_idx and end_idx is an i64, so incrementing cannot overflow.
                    Error::Unsupported("page label source index overflow".to_string())
                })?; // cov:ignore-end
                if !tree.has_index(self.pdf, source_idx)? {
                    continue;
                }
                let Some(label) = self.get_label_for_page_from_tree(tree, source_idx)? else {
                    continue;
                };
                let output_idx = source_idx.checked_add(idx_offset).ok_or_else(|| {
                    Error::Unsupported("page label output index overflow".to_string())
                })?;
                labels.push((output_idx, label));
            }
        }
        Ok(())
    }

    fn get_label_for_page_from_tree(
        &mut self,
        tree: &mut crate::nntree::NumberTree,
        page_idx: i64,
    ) -> Result<Option<ObjectHandle>> {
        let Some((label, offset)) = tree.find_object_at_or_below(self.pdf, page_idx)? else {
            return Ok(None);
        };
        let label = self.pdf.resolve_to_terminal(&label)?;
        if label.try_as_dictionary()?.is_none() {
            return Ok(None);
        }

        let start = label
            .try_get_key(b"/St")?
            .try_as_integer()?
            .unwrap_or(1)
            .checked_add(offset)
            .ok_or_else(|| Error::Unsupported("page label /St offset overflow".to_string()))?;
        let result = ObjectHandle::dictionary(Vec::new());
        result.replace_key(b"/S", label.try_get_key(b"/S")?)?;
        result.replace_key(b"/P", label.try_get_key(b"/P")?)?;
        result.replace_key(b"/St", ObjectHandle::integer(start))?;
        Ok(Some(result))
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
    // qpdf-deviation-start: page-label rendering has no qpdf counterpart --
    // see the marker on LabelRange::format for the full citation.
    pub fn label_string_for_page(&mut self, page_idx: i64) -> Result<String> {
        match self.label_for_page(page_idx)? {
            Some(effective) => Ok(effective.format(effective.start)),
            None => Ok((page_idx + 1).to_string()),
        }
    }
    // qpdf-deviation-end

    /// qpdf `getLabelsForPageRange` port: collect the label entries needed to
    /// reproduce the labels of pages `start_idx..=end_idx` if they were
    /// renumbered to begin at `new_start_idx`. Returns `(new_index, LabelRange)`
    /// pairs (the first entry plus every explicit entry in the source range),
    /// renumbered by `new_start_idx - start_idx`. Read-only; intended for
    /// page-extraction/subsetting call sites that reconstruct a document's
    /// `/PageLabels` for a new page range (pair with
    /// [`PageLabelDocumentHelper::write_reconstructed_labels`]).
    ///
    /// `start_idx` must be `<= end_idx`. An inverted span (`start_idx >
    /// end_idx`) is a caller bug: this returns only the first-page label
    /// with no explicit-range entries, matching an empty-span read; it does
    /// not panic. `start_idx == end_idx` denotes a single-page span.
    ///
    /// `new_start_idx` is expected to be a valid page-index-shaped value
    /// (typically `0..page_count`). Arithmetic that cannot be represented as
    /// an `i64` returns [`crate::Error::Unsupported`].
    ///
    /// Unlike qpdf's accumulating signature, this is a single self-contained
    /// call: the leading entry is always emitted (the result vector starts
    /// empty, so there is no prior entry to be redundant against). A later
    /// accumulating consumer can dedupe across calls.
    ///
    /// Traverses the `/PageLabels` tree once for this call and keeps raw
    /// dictionary handles until the typed compatibility view is built.
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
        // debug_assert catches an inverted span (a caller bug per the doc
        // contract) in dev/tests. Release keeps the graceful empty-span
        // behavior documented above.
        debug_assert!(
            start_idx <= end_idx,
            "labels_for_page_range: inverted span (start={start_idx}, end={end_idx})",
        );
        let mut raw_labels = Vec::new();
        self.get_labels_for_page_range(start_idx, end_idx, new_start_idx, &mut raw_labels)?;
        raw_labels
            .into_iter()
            .map(|(index, label)| {
                LabelRange::from_handle(self.pdf, &label)?
                    .map(|label| (index, label))
                    .ok_or_else(|| {
                        // cov:ignore-start: get_labels_for_page_range only stores direct dictionary handles in raw_labels.
                        Error::Unsupported("page label range is not a dictionary".to_string())
                    }) // cov:ignore-end
            })
            .collect()
    }

    /// Batch variant of [`Self::labels_for_page_range`] for
    /// page-selection/split/merge callers that would otherwise re-parse the
    /// `/PageLabels` tree once per selected page. Fetches the tree ONCE and
    /// emits one entry per input index (in input order); each entry's output
    /// index is `out_start_idx + i`, so multi-input mergers can pass a
    /// running base. Pair with [`merge_adjacent_ranges`] to fold away
    /// redundant tail entries before writing.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded while reading the existing tree.
    /// - Any error from [`Pdf::resolve`].
    pub fn labels_for_selection(
        &mut self,
        src_indices: &[i64],
        out_start_idx: i64,
    ) -> Result<Vec<(i64, LabelRange)>> {
        self.labels_for_selection_with_prefix_presence(src_indices, out_start_idx)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|(index, label, _prefix_present)| (index, label))
                    .collect()
            })
    }

    /// Batch variant of [`Self::labels_for_selection`] that also returns
    /// whether each effective label dictionary carries an explicit `/P` key.
    ///
    /// The raw label lookup and the typed [`LabelRange`] projection happen in
    /// one pass. Callers that need to preserve the distinction between an
    /// absent prefix and an explicit empty `/P ()` should use this method so
    /// they do not traverse the number tree again for each selected page.
    /// This follows qpdf's `getLabelForPage` raw-dictionary construction
    /// (`QPDFPageLabelDocumentHelper.cc:23-51`), which retains `/P` key
    /// presence while projecting the effective `/St` value.
    pub fn labels_for_selection_with_prefix_presence(
        &mut self,
        src_indices: &[i64],
        out_start_idx: i64,
    ) -> Result<Vec<(i64, LabelRange, bool)>> {
        let mut tree = self.pagelabels_tree()?;
        let mut out = Vec::with_capacity(src_indices.len());
        for (i, &src_idx) in src_indices.iter().enumerate() {
            let out_idx = out_start_idx
                .checked_add(i64::try_from(i).map_err(|_| {
                    // cov:ignore-start: supported 64-bit targets cannot allocate a slice with more than i64::MAX elements.
                    Error::Unsupported("page label selection index overflow".to_string())
                })?) // cov:ignore-end
                .ok_or_else(|| {
                    Error::Unsupported("page label output index overflow".to_string())
                })?;
            let label = match tree.as_mut() {
                Some(tree) => self.get_label_for_page_from_tree(tree, src_idx)?,
                None => None,
            };
            let (label, prefix_present) = match label {
                Some(label) => {
                    let prefix_present = label.try_has_key(b"/P")?;
                    let label = LabelRange::from_handle(self.pdf, &label)?.ok_or_else(|| {
                        // cov:ignore-start: get_label_for_page_from_tree returns Some only for a dictionary handle.
                        Error::Unsupported("page label range is not a dictionary".to_string())
                    })?; // cov:ignore-end
                    (label, prefix_present)
                }
                None => (
                    LabelRange {
                        style: LabelStyle::None,
                        prefix: String::new(),
                        start: out_idx.checked_add(1).ok_or_else(|| {
                            Error::Unsupported("page label fabricated start overflow".to_string())
                        })?,
                    },
                    false,
                ),
            };
            out.push((out_idx, label, prefix_present));
        }
        Ok(out)
    }

    /// Return whether the effective label dictionary for `page_idx` carries
    /// an explicit `/P` key, including an explicitly empty string. qpdf keeps
    /// that distinction when `getLabelsForPageRange` copies raw label
    /// dictionaries; the JSON representation renders an empty Unicode prefix
    /// as `u:` while an absent prefix is omitted.
    pub fn label_prefix_is_present(&mut self, page_idx: i64) -> Result<bool> {
        let Some(mut tree) = self.pagelabels_tree()? else {
            return Ok(false);
        };
        let Some(label) = self.get_label_for_page_from_tree(&mut tree, page_idx)? else {
            return Ok(false);
        };
        let label = self.pdf.resolve_to_terminal(&label)?;
        label.try_has_key(b"/P")
    }

    /// Install `entries` as the catalog's `/PageLabels`: a direct
    /// (non-indirect) `<< /Nums [...] >>` dictionary — never a balanced
    /// number tree — with each entry's label dictionary built in the same
    /// shape qpdf's `getLabelForPage` reconstruction uses (`/S` iff the style
    /// is not [`LabelStyle::None`], `/P` iff the prefix is non-empty, `/St`
    /// always present).
    ///
    /// This is the shape qpdf produces when reconstructing labels for a page
    /// subset or split (`QPDFJob::handlePageSpecs`, `QPDFJob::doSplitPages`):
    /// it always unconditionally replaces the catalog's `/PageLabels` with a
    /// freshly built flat array — never merging with, or preserving the
    /// shape of, any prior value. Pair with [`Self::labels_for_page_range`] /
    /// [`Self::label_for_page`], which produce the `entries` this expects.
    /// A no-op when the document has no catalog, or the catalog is not a
    /// dictionary.
    ///
    /// # Errors
    ///
    /// Any error from [`Pdf::resolve`].
    pub fn write_reconstructed_labels(&mut self, entries: &[(i64, LabelRange)]) -> Result<()> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(());
        };
        let catalog = self.pdf.get_object_handle(catalog_ref);
        catalog.try_dereference()?;
        if catalog.try_as_dictionary()?.is_none() {
            return Ok(());
        }
        let mut nums = Vec::with_capacity(entries.len() * 2);
        for (idx, range) in entries {
            nums.push(ObjectHandle::integer(*idx));
            nums.push(Self::reconstructed_label_handle(range, false)?);
        }
        let page_labels =
            ObjectHandle::dictionary(vec![(b"/Nums".to_vec(), ObjectHandle::array(nums))]);
        catalog.replace_key(b"/PageLabels", page_labels)?;
        self.pdf.mark_object_handle_dirty(&catalog)?;
        Ok(())
    }

    /// Install reconstructed labels while retaining whether the source had an
    /// explicit empty `/P` prefix. This is the raw-handle distinction qpdf's
    /// `getLabelsForPageRange` preserves and the compact [`LabelRange`] value
    /// intentionally does not expose in its public three-field shape.
    pub fn write_reconstructed_labels_with_prefix_presence(
        &mut self,
        entries: &[(i64, LabelRange, bool)],
    ) -> Result<()> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(());
        };
        let catalog = self.pdf.get_object_handle(catalog_ref);
        catalog.try_dereference()?;
        if catalog.try_as_dictionary()?.is_none() {
            return Ok(());
        }
        let mut nums = Vec::with_capacity(entries.len() * 2);
        for (idx, range, prefix_present) in entries {
            nums.push(ObjectHandle::integer(*idx));
            nums.push(Self::reconstructed_label_handle(range, *prefix_present)?);
        }
        let page_labels =
            ObjectHandle::dictionary(vec![(b"/Nums".to_vec(), ObjectHandle::array(nums))]);
        catalog.replace_key(b"/PageLabels", page_labels)?;
        self.pdf.mark_object_handle_dirty(&catalog)?;
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
    use crate::json::Json;
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

    fn install_page_labels(pdf: &mut Pdf<Cursor<Vec<u8>>>, value: ObjectHandle) {
        let catalog_ref = pdf.root_ref().expect("one-page fixture has a catalog root");
        let catalog = pdf.get_object_handle(catalog_ref);
        pdf.resolve(&catalog).expect("resolve catalog");
        catalog
            .replace_key(b"/PageLabels", value)
            .expect("install catalog page labels");
        pdf.mark_object_handle_dirty(&catalog)
            .expect("mark catalog dirty");
    }

    fn pdf_with_pagelabels(nums: Vec<ObjectHandle>) -> Pdf<Cursor<Vec<u8>>> {
        // Minimal one-page PDF; then attach an indirect /PageLabels leaf via
        // the canonical handle allocation and catalog mutation boundary.
        let mut pdf = bare_one_page_pdf();
        let leaf = ObjectHandle::dictionary(vec![(b"/Nums".to_vec(), ObjectHandle::array(nums))]);
        let leaf = pdf
            .make_indirect_from_object_handle(leaf)
            .expect("promote page-label leaf");
        install_page_labels(&mut pdf, leaf);
        pdf
    }

    fn pdf_with_catalog_pagelabels(value: ObjectHandle) -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf = bare_one_page_pdf();
        install_page_labels(&mut pdf, value);
        pdf
    }

    fn label_dict(style: &str, st: Option<i64>, prefix: Option<&str>) -> ObjectHandle {
        let mut entries = vec![(
            b"/S".to_vec(),
            ObjectHandle::name(style.as_bytes().to_vec()),
        )];
        if let Some(s) = st {
            entries.push((b"/St".to_vec(), ObjectHandle::integer(s)));
        }
        if let Some(p) = prefix {
            entries.push((b"/P".to_vec(), ObjectHandle::string(p.as_bytes().to_vec())));
        }
        ObjectHandle::dictionary(entries)
    }

    #[test]
    fn label_string_multi_range_matches_spec() {
        // /Nums [0 <</S /r>> 3 <</S /D /St 1>> 6 <</S /D /P "A-" /St 1>>]
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("r", None, None),
            ObjectHandle::integer(3),
            label_dict("D", Some(1), None),
            ObjectHandle::integer(6),
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
    fn ranges_does_not_dirty_a_valid_indirect_tree() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(1), None),
        ]);
        for object_ref in pdf.dirty_object_refs() {
            pdf.clear_dirty(object_ref);
        }

        let ranges = pdf.page_labels().ranges().expect("read ranges");

        assert_eq!(ranges.len(), 1);
        assert!(pdf.dirty_object_refs().is_empty());
    }

    #[test]
    fn label_for_page_offsets_start() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(10), None),
        ]);
        let mut h = pdf.page_labels();
        let lab = h.label_for_page(2).unwrap().expect("range applies");
        assert_eq!(lab.start, 12, "/St 10 + offset 2");
        assert_eq!(lab.style, LabelStyle::Decimal);
    }

    #[test]
    fn get_label_for_page_preserves_qpdf_raw_dictionary_keys() {
        let unknown_with_empty_prefix = ObjectHandle::dictionary(vec![
            (b"/S".to_vec(), ObjectHandle::name(b"Z".to_vec())),
            (b"/P".to_vec(), ObjectHandle::string(Vec::new())),
            (b"/St".to_vec(), ObjectHandle::integer(3)),
        ]);
        let decimal_without_prefix =
            ObjectHandle::dictionary(vec![(b"/S".to_vec(), ObjectHandle::name(b"D".to_vec()))]);

        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            unknown_with_empty_prefix,
            ObjectHandle::integer(2),
            decimal_without_prefix,
        ]);
        let mut h = pdf.page_labels();

        let unknown = h
            .get_label_for_page(1)
            .unwrap()
            .expect("unknown style range applies");
        assert_eq!(
            unknown.try_get_key(b"/S").unwrap().try_as_name().unwrap(),
            Some(b"Z".to_vec())
        );
        assert!(unknown.try_has_key(b"/P").unwrap(), "empty /P is present");
        assert_eq!(
            unknown.try_get_key(b"/P").unwrap().as_string(),
            Some(vec![])
        );
        assert_eq!(unknown.try_get_key(b"/St").unwrap().as_integer(), Some(4));

        let decimal = h
            .get_label_for_page(2)
            .unwrap()
            .expect("decimal range applies");
        assert_eq!(
            decimal.try_get_key(b"/S").unwrap().try_as_name().unwrap(),
            Some(b"D".to_vec())
        );
        assert!(
            !decimal.try_has_key(b"/P").unwrap(),
            "absent /P stays absent"
        );
        assert_eq!(decimal.try_get_key(b"/St").unwrap().as_integer(), Some(1));
    }

    #[test]
    fn get_label_for_page_preserves_indirect_member_identity() {
        let mut pdf = bare_one_page_pdf();
        let label = pdf
            .make_indirect_from_object_handle(label_dict("D", None, Some("prefix")))
            .expect("promote label dictionary");
        let leaf = ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::integer(0), label.clone()]),
        )]);
        let leaf = pdf
            .make_indirect_from_object_handle(leaf)
            .expect("promote page-label leaf");
        install_page_labels(&mut pdf, leaf);

        let source = label.clone();
        pdf.resolve(&source).unwrap();
        let source_style = source.try_get_key(b"/S").unwrap();
        let source_prefix = source.try_get_key(b"/P").unwrap();

        let mut helper = pdf.page_labels();
        let result = helper
            .get_label_for_page(0)
            .unwrap()
            .expect("label dictionary");

        assert!(result
            .try_get_key(b"/S")
            .unwrap()
            .is_same_object_as(&source_style));
        assert!(result
            .try_get_key(b"/P")
            .unwrap()
            .is_same_object_as(&source_prefix));
    }

    #[test]
    fn page_label_dict_matches_qpdf_factory_shape() {
        let label = PageLabelDocumentHelper::<Cursor<Vec<u8>>>::page_label_dict(
            LabelStyle::AlphaUpper,
            3,
            "§",
        );
        assert_eq!(
            label.try_get_key(b"/S").unwrap().try_as_name().unwrap(),
            Some(b"A".to_vec())
        );
        assert_eq!(
            label.try_get_key(b"/P").unwrap().as_string(),
            Some(vec![0xa7])
        );
        assert_eq!(label.try_get_key(b"/St").unwrap().as_integer(), Some(3));

        let ascii_prefix = PageLabelDocumentHelper::<Cursor<Vec<u8>>>::page_label_dict(
            LabelStyle::Decimal,
            1,
            "A-",
        );
        assert_eq!(
            ascii_prefix.try_get_key(b"/P").unwrap().as_string(),
            Some(b"A-".to_vec())
        );

        let default =
            PageLabelDocumentHelper::<Cursor<Vec<u8>>>::page_label_dict(LabelStyle::None, 1, "");
        assert!(!default.try_has_key(b"/S").unwrap());
        assert!(!default.try_has_key(b"/P").unwrap());
        assert!(!default.try_has_key(b"/St").unwrap());
    }

    #[test]
    fn get_labels_for_page_range_accumulates_effective_raw_entries() {
        let empty_prefix = ObjectHandle::dictionary(vec![
            (b"/S".to_vec(), ObjectHandle::name(b"D".to_vec())),
            (b"/P".to_vec(), ObjectHandle::string(Vec::new())),
        ]);
        let decimal = ObjectHandle::dictionary(vec![
            (b"/S".to_vec(), ObjectHandle::name(b"D".to_vec())),
            (b"/St".to_vec(), ObjectHandle::integer(10)),
        ]);

        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("r", None, None),
            ObjectHandle::integer(2),
            empty_prefix,
            ObjectHandle::integer(4),
            decimal,
        ]);
        let mut h = pdf.page_labels();
        let mut labels = Vec::new();
        h.get_labels_for_page_range(1, 4, 0, &mut labels).unwrap();

        assert_eq!(labels.len(), 3);
        assert_eq!(labels[0].0, 0);
        assert_eq!(
            labels[0]
                .1
                .try_get_key(b"/S")
                .unwrap()
                .try_as_name()
                .unwrap(),
            Some(b"r".to_vec())
        );
        assert_eq!(
            labels[0].1.try_get_key(b"/St").unwrap().as_integer(),
            Some(2)
        );
        assert_eq!(labels[1].0, 1);
        assert!(labels[1].1.try_has_key(b"/P").unwrap());
        assert_eq!(
            labels[1].1.try_get_key(b"/St").unwrap().as_integer(),
            Some(1)
        );
        assert_eq!(labels[2].0, 3);
        assert_eq!(
            labels[2].1.try_get_key(b"/St").unwrap().as_integer(),
            Some(10)
        );
    }

    #[test]
    fn get_labels_for_page_range_skips_redundant_accumulated_leading_entry() {
        let mut pdf =
            pdf_with_pagelabels(vec![ObjectHandle::integer(0), label_dict("D", None, None)]);
        let mut h = pdf.page_labels();
        let prior =
            PageLabelDocumentHelper::<Cursor<Vec<u8>>>::page_label_dict(LabelStyle::Decimal, 1, "");
        prior.replace_key(b"/St", ObjectHandle::integer(1)).unwrap();
        let mut labels = vec![(0, prior)];

        h.get_labels_for_page_range(1, 1, 1, &mut labels).unwrap();

        assert_eq!(labels.len(), 1, "the leading continuation is redundant");
    }

    #[test]
    fn get_labels_for_page_range_handles_missing_tree_and_non_dictionary_prior() {
        let mut pdf = bare_one_page_pdf();
        let mut h = pdf.page_labels();
        let mut labels = vec![(0, ObjectHandle::integer(0))];

        h.get_labels_for_page_range(0, 0, 0, &mut labels).unwrap();

        assert_eq!(labels.len(), 2);
        assert_eq!(labels[1].0, 0);
        assert_eq!(
            labels[1].1.try_get_key(b"/St").unwrap().as_integer(),
            Some(1)
        );
    }

    #[test]
    fn get_labels_for_page_range_rejects_fabricated_start_overflow() {
        let mut pdf = bare_one_page_pdf();
        let error = pdf
            .page_labels()
            .get_labels_for_page_range(i64::MAX, i64::MAX, i64::MAX, &mut Vec::new())
            .expect_err("fabricated /St must use checked arithmetic");
        assert!(error.to_string().contains("fabricated start overflow"));
    }

    #[test]
    fn get_labels_for_page_range_skips_non_dictionary_explicit_entries() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", None, None),
            ObjectHandle::integer(1),
            ObjectHandle::integer(99),
        ]);
        let mut h = pdf.page_labels();
        let mut labels = Vec::new();

        h.get_labels_for_page_range(0, 2, 0, &mut labels).unwrap();

        assert_eq!(
            labels.len(),
            1,
            "a non-dictionary explicit value is skipped"
        );
        assert_eq!(labels[0].0, 0);
    }

    #[test]
    fn get_labels_for_page_range_rejects_output_index_overflow() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(1), None),
            ObjectHandle::integer(1),
            label_dict("R", Some(1), None),
        ]);
        let error = pdf
            .page_labels()
            .get_labels_for_page_range(0, 2, i64::MAX, &mut Vec::new())
            .expect_err("reconstructed output index must use checked arithmetic");
        assert!(error.to_string().contains("output index overflow"));
    }

    #[test]
    fn labels_for_selection_reconstructs_effective_and_default_ranges() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(5),
            label_dict("R", Some(2), Some("P-")),
        ]);
        let mut h = pdf.page_labels();

        let labels = h.labels_for_selection(&[0, 5, 6], 10).unwrap();

        assert_eq!(labels[0], (10, none_range(11)));
        assert_eq!(
            labels[1],
            (
                11,
                LabelRange {
                    style: LabelStyle::RomanUpper,
                    prefix: "P-".into(),
                    start: 2,
                }
            )
        );
        assert_eq!(
            labels[2],
            (
                12,
                LabelRange {
                    style: LabelStyle::RomanUpper,
                    prefix: "P-".into(),
                    start: 3,
                }
            )
        );
    }

    #[test]
    fn labels_for_selection_with_prefix_presence_preserves_raw_prefix_presence() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(1), Some("")),
            ObjectHandle::integer(1),
            label_dict("D", Some(1), None),
        ]);
        let labels = pdf
            .page_labels()
            .labels_for_selection_with_prefix_presence(&[0, 1], 0)
            .expect("selection labels with prefix presence");

        assert_eq!(
            labels,
            vec![
                (
                    0,
                    LabelRange {
                        style: LabelStyle::Decimal,
                        prefix: String::new(),
                        start: 1,
                    },
                    true,
                ),
                (
                    1,
                    LabelRange {
                        style: LabelStyle::Decimal,
                        prefix: String::new(),
                        start: 1,
                    },
                    false,
                ),
            ]
        );
    }

    #[test]
    fn labels_for_selection_handles_missing_tree() {
        let mut pdf = bare_one_page_pdf();
        let labels = pdf.page_labels().labels_for_selection(&[0], 0).unwrap();
        assert_eq!(labels, vec![(0, none_range(1))]);
    }

    #[test]
    fn labels_for_selection_rejects_output_index_overflow() {
        let mut pdf =
            pdf_with_pagelabels(vec![ObjectHandle::integer(0), label_dict("D", None, None)]);
        let error = pdf
            .page_labels()
            .labels_for_selection(&[0, 1], i64::MAX)
            .expect_err("selection output index must use checked arithmetic");
        assert!(error.to_string().contains("output index overflow"));
    }

    #[test]
    fn labels_for_selection_rejects_fabricated_start_overflow() {
        let mut pdf = bare_one_page_pdf();
        let error = pdf
            .page_labels()
            .labels_for_selection(&[0], i64::MAX)
            .expect_err("fabricated selection /St must use checked arithmetic");
        assert!(error.to_string().contains("fabricated start overflow"));
    }

    #[test]
    fn get_label_for_page_rejects_checked_start_overflow() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(i64::MAX), None),
        ]);
        let error = pdf
            .page_labels()
            .get_label_for_page(1)
            .expect_err("/St offset must use checked arithmetic");
        assert!(error.to_string().contains("offset overflow"));
    }

    #[test]
    fn get_label_for_page_reports_qpdf_short_number_tree_error() {
        let mut pdf = pdf_with_pagelabels(vec![ObjectHandle::integer(0)]);
        let error = pdf
            .page_labels()
            .get_label_for_page(0)
            .expect_err("qpdf rejects a short /Nums pair during find");
        assert!(error.to_string().contains("items array is too short"));
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|warning| warning
                .message
                .contains("update ivalue: items array is too short")));
    }

    #[test]
    fn get_label_for_page_ignores_non_dictionary_values() {
        let mut pdf =
            pdf_with_pagelabels(vec![ObjectHandle::integer(0), ObjectHandle::integer(99)]);
        assert!(pdf.page_labels().get_label_for_page(0).unwrap().is_none());
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
    fn direct_null_page_labels_key_is_absent() {
        let mut pdf = pdf_with_catalog_pagelabels(ObjectHandle::null());
        let mut helper = pdf.page_labels();

        assert!(!helper.has_page_labels().unwrap());
        assert!(helper.get_label_for_page(0).unwrap().is_none());
        assert_eq!(helper.label_string_for_page(0).unwrap(), "1");
        assert!(helper.ranges().unwrap().is_empty());
    }

    #[test]
    fn indirect_null_page_labels_key_is_absent() {
        let mut pdf = bare_one_page_pdf();
        let null_handle = pdf
            .make_indirect_from_object_handle(ObjectHandle::null())
            .expect("promote null page-label value");
        install_page_labels(&mut pdf, null_handle);
        let mut helper = pdf.page_labels();

        assert!(!helper.has_page_labels().unwrap());
        assert!(helper.get_label_for_page(0).unwrap().is_none());
        assert_eq!(helper.label_string_for_page(0).unwrap(), "1");
        assert!(helper.ranges().unwrap().is_empty());
    }

    #[test]
    fn page_before_first_range_defaults_to_decimal() {
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(3),
            label_dict("R", Some(1), None),
        ]);
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
            ObjectHandle::integer(0),
            label_dict("r", Some(1), None),
            ObjectHandle::integer(5),
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
    fn ranges_resolves_indirect_inner_st() {
        // Put an indirect /St value in a live label dictionary.
        let mut pdf = bare_one_page_pdf();
        let st = pdf
            .make_indirect_from_object_handle(ObjectHandle::integer(7))
            .expect("promote /St value");
        let label = label_dict("D", None, None);
        label.replace_key(b"/St", st).unwrap();
        let leaf = ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::integer(0), label]),
        )]);
        let leaf = pdf
            .make_indirect_from_object_handle(leaf)
            .expect("promote page-label leaf");
        install_page_labels(&mut pdf, leaf);
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].1.start, 7, "indirect /St must be resolved");
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
    fn ranges_handles_indirect_and_non_dict_values() {
        // entry 0: indirect ref to a label dict; entry 5: a non-dict value (skipped).
        let mut pdf = bare_one_page_pdf();
        let lab = pdf
            .make_indirect_from_object_handle(label_dict("D", None, None))
            .expect("promote indirect label dictionary");
        let leaf = ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![
                ObjectHandle::integer(0),
                lab, // indirect entry value -> resolve
                ObjectHandle::integer(5),
                ObjectHandle::integer(99), // non-dict entry value -> skipped
            ]),
        )]);
        let leaf = pdf
            .make_indirect_from_object_handle(leaf)
            .expect("promote page-label leaf");
        install_page_labels(&mut pdf, leaf);
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1, "non-dict value skipped");
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[0].1.style, LabelStyle::Decimal);
    }

    #[test]
    fn ranges_non_name_style_resolves_to_none() {
        // A label dict whose /S is not a Name maps to LabelStyle::None via
        // the canonical handle-to-typed compatibility view.
        let mut pdf = bare_one_page_pdf();
        let lab = ObjectHandle::dictionary(vec![
            (b"/S".to_vec(), ObjectHandle::integer(0)), // non-Name /S
        ]);
        let leaf = ObjectHandle::dictionary(vec![(
            b"/Nums".to_vec(),
            ObjectHandle::array(vec![ObjectHandle::integer(0), lab]),
        )]);
        let leaf = pdf
            .make_indirect_from_object_handle(leaf)
            .expect("promote page-label leaf");
        install_page_labels(&mut pdf, leaf);
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].1.style, LabelStyle::None);
    }

    #[test]
    fn labels_for_page_range_fabricates_default_when_first_unlabeled() {
        // Only an explicit range at index 5; extract starting before it (idx 0).
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(5),
            label_dict("D", Some(1), None),
        ]);
        let mut h = pdf.page_labels();
        let out = h.labels_for_page_range(0, 6, 0).unwrap();
        // First entry fabricated: no /S (LabelStyle::None), start = new_start(0) + 1 = 1.
        // Matches qpdf 11.9.0's own fabricated dict (`getLabelForPage` returning
        // null): `<< /St 1 >>`, no `/S /D`.
        assert_eq!(out[0].0, 0);
        assert_eq!(out[0].1.style, LabelStyle::None);
        assert_eq!(out[0].1.start, 1);
        // The explicit range at 5 is copied (renumbered to 5).
        assert!(out.iter().any(|(idx, _)| *idx == 5));
    }

    #[test]
    fn labels_for_page_range_single_page_span_does_not_panic() {
        // start_idx == end_idx (a single-page span, the common case for
        // --split-pages=1 or a chunk's last page) must not panic: the
        // internal `explicit.range((start_idx+1)..=end_idx)` bound would
        // otherwise be inverted, which `BTreeSet::range` panics on.
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(1), None),
        ]);
        let mut h = pdf.page_labels();
        let out = h.labels_for_page_range(4, 4, 0).unwrap();
        assert_eq!(out, vec![(0, dec(5))], "St 1 + offset 4 = 5");
    }

    #[test]
    fn helper_tolerates_non_dict_catalog() {
        let mut pdf = pdf_with_pagelabels(vec![]);
        let catalog_ref = pdf.root_ref().unwrap();
        pdf.replace_object_handle(catalog_ref, ObjectHandle::integer(0))
            .unwrap(); // catalog no longer a dict
        let mut h = pdf.page_labels();
        assert!(
            !h.has_page_labels().unwrap(),
            "non-dict catalog => no labels"
        );
        assert_eq!(h.ranges().unwrap(), vec![]);
    }

    #[test]
    fn helper_tolerates_missing_root() {
        // A trailer without /Root makes root_ref() return None; the helper must
        // degrade gracefully (no labels, reconstruction is a no-op).
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
    }

    /// Shorthand for a plain decimal range starting at `start`, no prefix.
    fn dec(start: i64) -> LabelRange {
        LabelRange {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            start,
        }
    }

    /// Shorthand for a style-less range (no `/S`), no prefix, starting at
    /// `start` — the shape of qpdf's fabricated "unlabeled page" default.
    fn none_range(start: i64) -> LabelRange {
        LabelRange {
            style: LabelStyle::None,
            prefix: String::new(),
            start,
        }
    }

    #[test]
    fn write_reconstructed_labels_installs_direct_nums_dict() {
        // Installed as a direct dict — not an indirect number tree — and
        // every entry's /St is explicit (qpdf --split-pages/--pages parity).
        let mut pdf = bare_one_page_pdf();
        {
            let mut h = pdf.page_labels();
            h.write_reconstructed_labels(&[
                (0, none_range(1)),
                (
                    3,
                    LabelRange {
                        style: LabelStyle::Decimal,
                        prefix: String::new(),
                        start: 1,
                    },
                ),
            ])
            .unwrap();
        }
        let catalog_ref = pdf.root_ref().unwrap();
        let catalog = pdf.get_object_handle(catalog_ref);
        pdf.resolve(&catalog).unwrap();
        let page_labels = catalog.get_key(b"/PageLabels");
        assert!(page_labels.as_dictionary().is_some());
        assert!(!page_labels.is_indirect(), "/PageLabels must be direct");
        let nums = page_labels.get_key(b"/Nums");
        let nums = nums.as_array().expect("/Nums must be a direct array");
        assert_eq!(nums.len(), 4, "2 entries * (index, dict)");
        assert_eq!(nums[0].as_integer(), Some(0));
        assert_eq!(nums[1].get_key(b"/St").as_integer(), Some(1));
        assert!(!nums[1].try_has_key(b"/S").unwrap());
        assert_eq!(nums[2].as_integer(), Some(3));
        assert_eq!(
            nums[3].get_key(b"/S").try_as_name().unwrap(),
            Some(b"D".to_vec())
        );
        assert_eq!(nums[3].get_key(b"/St").as_integer(), Some(1));

        // The high-level reader round-trips the installed entries too.
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[1].0, 3);
    }

    #[test]
    fn write_reconstructed_labels_preserves_catalog_source_metadata() {
        let mut pdf = bare_one_page_pdf();
        let catalog_ref = pdf.root_ref().expect("catalog ref");
        let catalog = pdf.get_object_handle(catalog_ref);
        catalog.try_dereference().expect("resolve catalog");
        let source_description = catalog.description();
        let source_end_offsets = catalog.end_offsets();
        assert!(
            source_end_offsets.0 >= 0 && source_end_offsets.1 >= 0,
            "fixture must establish catalog source extents"
        );

        pdf.page_labels()
            .write_reconstructed_labels(&[(0, none_range(1))])
            .expect("canonical label write");

        assert_eq!(
            catalog.description(),
            source_description,
            "replaceKey must preserve the catalog object description"
        );
        assert_eq!(
            catalog.end_offsets(),
            source_end_offsets,
            "replaceKey must preserve the catalog source extents"
        );
        assert!(
            catalog
                .try_get_key(b"/PageLabels")
                .expect("PageLabels key")
                .try_as_dictionary()
                .expect("PageLabels dictionary")
                .is_some(),
            "the live catalog handle must observe the replacement"
        );
    }

    #[test]
    fn label_prefix_presence_distinguishes_empty_and_absent_prefixes() {
        let mut explicit_empty = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(1), Some("")),
        ]);
        let mut helper = explicit_empty.page_labels();
        assert!(helper.label_prefix_is_present(0).unwrap());

        let mut empty_tree = pdf_with_pagelabels(vec![]);
        assert!(!empty_tree.page_labels().label_prefix_is_present(0).unwrap());

        let mut absent = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("D", Some(1), None),
        ]);
        assert!(!absent.page_labels().label_prefix_is_present(0).unwrap());

        let mut no_tree = bare_one_page_pdf();
        assert!(!no_tree.page_labels().label_prefix_is_present(0).unwrap());
    }

    #[test]
    fn reconstructed_labels_can_retain_an_explicit_empty_prefix() {
        let mut pdf = bare_one_page_pdf();
        let range = LabelRange {
            style: LabelStyle::Decimal,
            prefix: String::new(),
            start: 1,
        };
        pdf.page_labels()
            .write_reconstructed_labels_with_prefix_presence(&[
                (0, range.clone(), true),
                (2, range, false),
            ])
            .unwrap();

        let catalog_ref = pdf.root_ref().unwrap();
        let catalog = pdf.get_object_handle(catalog_ref);
        pdf.resolve(&catalog).unwrap();
        let page_labels = catalog.get_key(b"/PageLabels");
        let nums = page_labels
            .get_key(b"/Nums")
            .as_array()
            .expect("PageLabels /Nums must be an array");
        assert_eq!(nums[1].get_key(b"/P").as_string(), Some(Vec::new()));
        assert!(!nums[3].try_has_key(b"/P").unwrap());
    }

    #[test]
    fn write_reconstructed_labels_replaces_existing_indirect_tree() {
        // A pre-existing indirect /PageLabels root is unconditionally replaced
        // by a fresh direct dict (qpdf never merges).
        let mut pdf = pdf_with_pagelabels(vec![
            ObjectHandle::integer(0),
            label_dict("R", Some(1), None),
        ]);
        {
            let mut h = pdf.page_labels();
            h.write_reconstructed_labels(&[(0, none_range(1))]).unwrap();
        }
        let catalog_ref = pdf.root_ref().unwrap();
        let catalog = pdf.get_object_handle(catalog_ref);
        pdf.resolve(&catalog).unwrap();
        let page_labels = catalog.get_key(b"/PageLabels");
        assert!(
            page_labels.as_dictionary().is_some() && !page_labels.is_indirect(),
            "/PageLabels must now be a direct dict, not the old indirect ref"
        );
    }

    #[test]
    fn write_reconstructed_labels_noop_without_root() {
        // A trailer without /Root must degrade gracefully, matching the same
        // tolerant style as the other reconstruction helper.
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
        let mut h = pdf.page_labels();
        h.write_reconstructed_labels(&[(0, none_range(1))]).unwrap();
        h.write_reconstructed_labels_with_prefix_presence(&[(0, none_range(1), false)])
            .unwrap();
    }

    #[test]
    fn write_reconstructed_labels_noop_on_non_dict_catalog() {
        let mut pdf = bare_one_page_pdf();
        let catalog_ref = pdf.root_ref().unwrap();
        pdf.replace_object_handle(catalog_ref, ObjectHandle::integer(0))
            .unwrap(); // catalog no longer a dict
        let mut h = pdf.page_labels();
        h.write_reconstructed_labels(&[(0, none_range(1))]).unwrap();
        h.write_reconstructed_labels_with_prefix_presence(&[(0, none_range(1), false)])
            .unwrap();
    }

    // ---- live-qpdf 11.9.0 oracle: get_label_for_page / get_labels_for_page_range ----
    //
    // `QPDFJob::doJSONPageLabels` (`QPDFJob.cc:1095-1116`) serializes exactly
    // `getLabelsForPageRange`'s entries with `getJSON(json_version)` and no
    // schema transformation, so `qpdf --json=2 --json-key=pagelabels` is a
    // faithful window onto the raw label dictionaries these two functions
    // build. This does not wire flpdf's own `--json` output (that CLI wiring
    // is `flpdf-q28i`'s scope) — it is a test-only observation of qpdf's
    // internal state via its own JSON serializer.
    //
    // `getLabelsForPageRange`'s `skip_first` redundancy-skip branch needs a
    // non-empty accumulator from a prior call, which `doJSONPageLabels`
    // never provides (it always starts from an empty `Vec`) — so no oracle
    // here exercises it. It stays covered by the existing hand-derived
    // `merge_adjacent_ranges`/`labels_for_page_range_*` unit tests above.

    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Resolve qpdf 11.9.0: prefer `/usr/bin/qpdf` on Linux (apt-installed,
    /// pinned by CI), otherwise resolve `qpdf` on `PATH` (CI installs the
    /// pinned build there on macOS/Windows; see `.github/workflows/ci.yml`'s
    /// "Add qpdf 11.9.0 to PATH" steps). Returns `None` — the caller skips —
    /// unless the resolved candidate reports exactly `qpdf version 11.9.0`,
    /// so a developer host without the pinned binary doesn't fail `cargo
    /// test`, and a differently-versioned `qpdf` on `PATH` doesn't produce
    /// a silently wrong oracle comparison.
    fn pinned_qpdf() -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        let candidates: &[&str] = &["/usr/bin/qpdf", "qpdf"];
        #[cfg(not(target_os = "linux"))]
        let candidates: &[&str] = &["qpdf"];

        for candidate in candidates {
            // cov:ignore-start: CI provides the pinned qpdf 11.9.0 binary on every candidate path; a launch failure, non-zero exit, or version mismatch only happens on a developer host missing it.
            let Ok(version) = Command::new(candidate).arg("--version").output() else {
                continue;
            };
            if !version.status.success() {
                continue;
            }
            // cov:ignore-end
            let first_line = String::from_utf8_lossy(&version.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            if first_line == "qpdf version 11.9.0" {
                return Some(PathBuf::from(candidate));
            } // cov:ignore: CI's pinned candidate always matches; the non-matching-version fallthrough only happens on a developer host with a different qpdf on PATH
        }
        None // cov:ignore: CI always resolves a pinned candidate above; only reached on a developer host missing qpdf entirely
    }

    /// A 7-page PDF whose `/PageLabels /Nums` covers, at indices 0-5: an
    /// `/S`-only range, a range with none of `/S`/`/P`/`/St`, an `/St`-only
    /// range, a `/P`-only range, an unrecognized `/S` name, and a range with
    /// an explicit empty `/P ()` alongside `/S`/`/St`. Page 6 has no
    /// explicit `/Nums` entry, exercising the `/St` offset-addition path
    /// (`QPDFPageLabelDocumentHelper.cc:38-40`).
    fn qpdf_pagelabels_probe_pdf() -> Vec<u8> {
        let npages = 7u32;
        let mut bodies: Vec<Vec<u8>> =
            vec![b"<< /Type /Catalog /Pages 2 0 R /PageLabels 10 0 R >>".to_vec()];
        let kids = (0..npages)
            .map(|i| format!("{} 0 R", 3 + i))
            .collect::<Vec<_>>()
            .join(" ");
        bodies.push(format!("<< /Type /Pages /Kids [{kids}] /Count {npages} >>").into_bytes());
        for _ in 0..npages {
            bodies.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_vec());
        }
        // obj 10 (/PageLabels), right after the 7 pages (obj 3..=9).
        bodies.push(
            b"<< /Nums [\
              0 << /S /r >> \
              1 << >> \
              2 << /St 9 >> \
              3 << /P (Ch-) >> \
              4 << /S /Z >> \
              5 << /S /D /P () /St 2 >> \
              ] >>"
                .to_vec(),
        );

        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(bodies.len());
        for (index, body) in bodies.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\nendobj\n");
        }
        let xref_start = bytes.len();
        let total = bodies.len() + 1;
        bytes.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for offset in &offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        bytes
    }

    /// Run `qpdf --json=2 --json-key=pagelabels` and return the parsed
    /// `"pagelabels"` array.
    fn qpdf_json_pagelabels(qpdf: &Path, path: &Path) -> Json {
        let qpdf = Command::new(qpdf)
            .arg("--json=2")
            .arg("--json-key=pagelabels")
            .arg(path)
            .output()
            .expect("run pinned qpdf pagelabels oracle");
        let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
        assert!(
            qpdf.status.success(),
            "qpdf --json=2 --json-key=pagelabels failed: {qpdf_stderr}"
        );
        let document = Json::parse(&qpdf.stdout).expect("parse qpdf JSON output");
        document.get_dict_item("pagelabels")
    }

    /// Compare a raw label dictionary built by flpdf against qpdf's JSON
    /// rendering of the same dictionary (`getJSON`'s `/S`/`/P`/`/St` shape).
    ///
    /// ASCII-only: qpdf's JSON writer switches non-UTF-8-representable text
    /// strings from a `u:` prefix to a base64 `b:` prefix, which this helper
    /// does not decode.
    fn assert_label_matches_qpdf_json(
        flpdf_label: &ObjectHandle,
        qpdf_label: &Json,
        context: &str,
    ) {
        let qpdf_s = qpdf_label.get_dict_item("/S");
        if qpdf_s.is_null() {
            assert!(
                !flpdf_label.try_has_key(b"/S").unwrap(),
                "{context}: /S must be absent"
            );
        } else {
            let name = qpdf_s.get_string().expect("qpdf /S is a JSON string");
            let expected = name.strip_prefix(b"/").expect("qpdf name starts with /");
            assert_eq!(
                flpdf_label
                    .try_get_key(b"/S")
                    .unwrap()
                    .try_as_name()
                    .unwrap()
                    .as_deref(),
                Some(expected),
                "{context}: /S mismatch"
            );
        }

        let qpdf_p = qpdf_label.get_dict_item("/P");
        if qpdf_p.is_null() {
            assert!(
                !flpdf_label.try_has_key(b"/P").unwrap(),
                "{context}: /P must be absent"
            );
        } else {
            let string = qpdf_p.get_string().expect("qpdf /P is a JSON string");
            let expected = string
                .strip_prefix(b"u:")
                .expect("fixture /P values are ASCII (qpdf u: prefix)");
            assert_eq!(
                flpdf_label
                    .try_get_key(b"/P")
                    .unwrap()
                    .as_string()
                    .as_deref(),
                Some(expected),
                "{context}: /P mismatch"
            );
        }

        let qpdf_st = qpdf_label
            .get_dict_item("/St")
            .get_number()
            .expect("qpdf /St is always present");
        let qpdf_st: i64 = std::str::from_utf8(&qpdf_st)
            .unwrap()
            .parse()
            .expect("qpdf /St is an integer");
        assert_eq!(
            flpdf_label.try_get_key(b"/St").unwrap().as_integer(),
            Some(qpdf_st),
            "{context}: /St mismatch"
        );
    }

    #[test]
    fn get_labels_for_page_range_matches_pinned_qpdf_json_oracle() {
        let Some(qpdf) = pinned_qpdf() else {
            // cov:ignore-start: CI provides the pinned qpdf binary; this is a developer-host skip
            eprintln!("pinned qpdf 11.9.0 unavailable; skipping pagelabels oracle");
            return;
            // cov:ignore-end
        };
        let bytes = qpdf_pagelabels_probe_pdf();
        let directory = tempfile::tempdir().expect("temporary qpdf oracle directory");
        let path = directory.path().join("pagelabels-probe.pdf");
        std::fs::write(&path, &bytes).expect("write qpdf oracle fixture");

        let pagelabels = qpdf_json_pagelabels(&qpdf, &path);
        let mut expected: Vec<(i64, Json)> = Vec::new();
        pagelabels.for_each_array_item(|entry| {
            let index = entry
                .get_dict_item("index")
                .get_number()
                .and_then(|bytes| std::str::from_utf8(&bytes).ok()?.parse::<i64>().ok())
                .expect("qpdf pagelabels[].index is an integer");
            expected.push((index, entry.get_dict_item("label")));
        });
        // Only explicit /Nums indices (0..=5) get an entry: getLabelsForPageRange
        // gates non-leading pages on hasIndex, unlike get_label_for_page's
        // inherited-range lookup. Page 6 has no explicit entry.
        assert_eq!(
            expected.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            (0..=5).collect::<Vec<_>>(),
            "qpdf pagelabels index list"
        );

        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open");
        let mut helper = pdf.page_labels();
        let mut actual: Vec<(i64, ObjectHandle)> = Vec::new();
        helper
            .get_labels_for_page_range(0, 6, 0, &mut actual)
            .expect("get_labels_for_page_range");

        assert_eq!(actual.len(), expected.len(), "entry count");
        for ((actual_index, actual_label), (expected_index, expected_label)) in
            actual.iter().zip(expected.iter())
        {
            assert_eq!(actual_index, expected_index, "index");
            assert_label_matches_qpdf_json(
                actual_label,
                expected_label,
                &format!("index {actual_index}"),
            );
        }
    }

    #[test]
    fn get_label_for_page_matches_pinned_qpdf_json_oracle_for_every_page() {
        let Some(qpdf) = pinned_qpdf() else {
            // cov:ignore-start: CI provides the pinned qpdf binary; this is a developer-host skip
            eprintln!("pinned qpdf 11.9.0 unavailable; skipping pagelabels oracle");
            return;
            // cov:ignore-end
        };
        let bytes = qpdf_pagelabels_probe_pdf();
        let directory = tempfile::tempdir().expect("temporary qpdf oracle directory");
        let input_path = directory.path().join("pagelabels-probe.pdf");
        std::fs::write(&input_path, &bytes).expect("write qpdf oracle fixture");

        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open");
        let mut helper = pdf.page_labels();

        for page_idx in 0i64..7 {
            let output_path = directory.path().join(format!("page-{page_idx}.pdf"));
            let extract = Command::new(&qpdf)
                .arg("--empty")
                .arg("--pages")
                .arg(&input_path)
                .arg((page_idx + 1).to_string()) // qpdf --pages page numbers are 1-based.
                .arg("--")
                .arg(&output_path)
                .output()
                .expect("run pinned qpdf page extraction");
            let extract_stderr = String::from_utf8_lossy(&extract.stderr);
            assert!(
                extract.status.success(),
                "qpdf page {page_idx} extraction failed: {extract_stderr}"
            );

            let entries = qpdf_json_pagelabels(&qpdf, &output_path);
            let mut expected_label = None;
            entries.for_each_array_item(|entry| {
                expected_label = Some(entry.get_dict_item("label"));
            });
            let expected_label =
                expected_label.expect("single-page extraction always has an effective label");

            let actual = helper
                .get_label_for_page(page_idx)
                .expect("get_label_for_page")
                .expect("every page in this fixture has an effective label");
            assert_label_matches_qpdf_json(&actual, &expected_label, &format!("page {page_idx}"));
        }
    }
}
