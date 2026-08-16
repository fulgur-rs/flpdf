//! qpdf correspondence: `QPDFPageLabelDocumentHelper.cc` canonical page-label access and reconstruction.
//!
//! [`PageLabelDocumentHelper`] reads, reconstructs, renders (ISO 32000-1
//! §12.4.2), and edits the catalog `/PageLabels` number tree. The qpdf-shaped
//! read methods retain live [`ObjectHandle`] values for raw `/S`, `/P`, and
//! `/St` semantics; [`LabelRange`] is the typed compatibility view used by
//! existing page-operation callers.

use crate::name_number_tree::DEFAULT_MAX_TREE_DEPTH;
use crate::{Dictionary, Error, Object, ObjectHandle, Pdf, Result};
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
    /// [`PageLabelDocumentHelper::ranges`], which reads the canonical
    /// `ObjectHandle` graph; this plain form is for the
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

    /// Decode a live qpdf-shaped label dictionary without materializing it as
    /// a legacy [`Object`]. Unknown `/S` names remain unknown to the raw
    /// handle, while this typed compatibility view retains the historical
    /// `LabelStyle::None` mapping.
    fn from_handle<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        handle: &ObjectHandle,
    ) -> Result<Option<Self>> {
        let handle = pdf.resolve_object_handle_to_terminal(handle)?;
        if handle.try_as_dictionary()?.is_none() {
            return Ok(None);
        }
        let style = pdf
            .resolve_object_handle_to_terminal(&handle.try_get_key(b"/S")?)?
            .try_as_name()?
            .map(|name| LabelStyle::from_name(&name))
            .unwrap_or(LabelStyle::None);
        let prefix = pdf
            .resolve_object_handle_to_terminal(&handle.try_get_key(b"/P")?)?
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

    /// Build a label dictionary mirroring qpdf `pageLabelDict`: `/S` name when
    /// the style is not [`LabelStyle::None`]; `/P` only when non-empty; `/St`
    /// only when `!= 1`.
    ///
    /// Prefixes use qpdf's `newUnicodeString` encoding: PDFDocEncoding when
    /// lossless, otherwise a PDF UTF-16BE text string. Emitting the Rust
    /// `String`'s raw UTF-8 bytes verbatim would be misread by
    /// PDFDocEncoding-only readers — a source `§` (`c2 a7`) would come back as
    /// `Â§`.
    pub fn to_dict(&self) -> Dictionary {
        let mut d = Dictionary::new();
        if let Some(name) = self.style.to_name() {
            d.insert("S", Object::Name(name.into()));
        }
        if !self.prefix.is_empty() {
            let bytes = crate::pdf_string::new_unicode_string(self.prefix.as_bytes());
            d.insert("P", Object::String(bytes));
        }
        if self.start != 1 {
            d.insert("St", Object::Integer(self.start));
        }
        d
    }

    /// Build a label dictionary in the shape qpdf's
    /// `QPDFPageLabelDocumentHelper::getLabelForPage` reconstruction produces:
    /// `/S` and `/P` are included on the same terms as [`Self::to_dict`], but
    /// `/St` is **always** present — never omitted for the default value `1`.
    ///
    /// Use this (via
    /// [`PageLabelDocumentHelper::write_reconstructed_labels`]) for entries
    /// coming from [`PageLabelDocumentHelper::labels_for_page_range`] / a page-subset or -split
    /// operation; use [`Self::to_dict`] for a directly authored range (qpdf's
    /// `--set-page-labels` shape), where the default `/St 1` is omitted for
    /// brevity.
    pub(crate) fn to_reconstructed_dict(&self) -> Dictionary {
        let mut d = self.to_dict();
        d.insert("St", Object::Integer(self.start));
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
        let catalog = self.pdf.resolve_object_handle_to_terminal(&catalog)?;
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

    fn pagelabels_tree(&mut self) -> Result<Option<crate::nntree::HandleNumberTree>> {
        Ok(self
            .pagelabels_root_handle()?
            .map(|root| crate::nntree::HandleNumberTree::new(root, DEFAULT_MAX_TREE_DEPTH)))
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

    /// All label ranges as `(first_page_index, LabelRange)`, ascending by index.
    /// Empty when `/PageLabels` is absent.
    ///
    /// # Errors
    ///
    /// - [`crate::Error::Unsupported`] when the number-tree depth limit is
    ///   exceeded.
    /// - Any error from [`Pdf::resolve`].
    pub fn ranges(&mut self) -> Result<Vec<(i64, LabelRange)>> {
        let Some(tree) = self.pagelabels_tree()? else {
            return Ok(vec![]);
        };
        let raw_entries = tree.entries(self.pdf)?;
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
        let Some(tree) = self.pagelabels_tree()? else {
            return Ok(None);
        };
        self.get_label_for_page_from_tree(&tree, page_idx)
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
        let tree = self.pagelabels_tree()?;
        let first_label = match tree.as_ref() {
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

        if let Some(tree) = tree.as_ref() {
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
        tree: &crate::nntree::HandleNumberTree,
        page_idx: i64,
    ) -> Result<Option<ObjectHandle>> {
        let Some((label, offset)) = tree.find_object_at_or_below(self.pdf, page_idx)? else {
            return Ok(None);
        };
        let label = self.pdf.resolve_object_handle_to_terminal(&label)?;
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
        let tree = self.pagelabels_tree()?;
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
            let label = match tree.as_ref() {
                Some(tree) => self.get_label_for_page_from_tree(tree, src_idx)?,
                None => None,
            };
            let label = match label {
                Some(label) => LabelRange::from_handle(self.pdf, &label)?.ok_or_else(|| {
                    // cov:ignore-start: get_label_for_page_from_tree returns Some only for a dictionary handle.
                    Error::Unsupported("page label range is not a dictionary".to_string())
                })?, // cov:ignore-end
                None => LabelRange {
                    style: LabelStyle::None,
                    prefix: String::new(),
                    start: out_idx.checked_add(1).ok_or_else(|| {
                        Error::Unsupported("page label fabricated start overflow".to_string())
                    })?,
                },
            };
            out.push((out_idx, label));
        }
        Ok(out)
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
    /// Contrast with [`Self::write_labels`], which instead rebuilds a
    /// balanced number tree (the shape used for directly authored ranges).
    ///
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
        let Some(mut catalog) = self.pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
            return Ok(());
        };
        let mut nums = Vec::with_capacity(entries.len() * 2);
        for (idx, range) in entries {
            nums.push(Object::Integer(*idx));
            nums.push(Object::Dictionary(range.to_reconstructed_dict()));
        }
        let mut page_labels = Dictionary::new();
        page_labels.insert("Nums", Object::Array(nums));
        catalog.insert("PageLabels", Object::Dictionary(page_labels));
        self.pdf
            .set_object(catalog_ref, Object::Dictionary(catalog));
        Ok(())
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
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(());
        };
        let Some(mut catalog) = self.pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
            return Ok(());
        };
        let mut tree = match catalog.get("PageLabels").cloned() {
            Some(root) => crate::NumberTree::new(root, true),
            None => crate::NumberTree::new_empty(self.pdf, true)?,
        };
        tree.set_max_depth(DEFAULT_MAX_TREE_DEPTH);
        tree.insert(
            self.pdf,
            first_page_idx,
            Object::Dictionary(range.to_dict()),
        )?;
        tree.make_root_indirect(self.pdf)?;
        catalog.insert("PageLabels", tree.into_root());
        self.pdf
            .set_object(catalog_ref, Object::Dictionary(catalog));
        Ok(())
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
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(false);
        };
        let Some(mut catalog) = self.pdf.resolve_borrowed(catalog_ref)?.as_dict().cloned() else {
            return Ok(false);
        };
        let Some(root) = catalog.get("PageLabels").cloned() else {
            return Ok(false);
        };
        let mut tree = crate::NumberTree::new(root, true);
        tree.set_max_depth(DEFAULT_MAX_TREE_DEPTH);
        if tree.remove(self.pdf, first_page_idx)?.is_none() {
            return Ok(false);
        }
        if tree.begin(self.pdf)?.valid() {
            tree.make_root_indirect(self.pdf)?;
            catalog.insert("PageLabels", tree.into_root());
        } else {
            catalog.remove("PageLabels");
        }
        self.pdf
            .set_object(catalog_ref, Object::Dictionary(catalog));
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
    /// given list (rebalanced through [`crate::NumberTree`] with qpdf's split
    /// behavior).
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

        // Compute the label that old source page `at` was showing — the
        // first surviving page (which will end up at output index `at +
        // count`) has to keep that label. Without this, an insertion inside
        // an existing range (or inside the default-decimal prefix) shifts
        // the effective numbering: `insert_pages(2, 1)` on a single decimal
        // range at 0 makes old page 2 (previously "3") render as "4"
        // instead of "3"; the same happens in the default prefix when
        // no explicit range covers `at`.
        let mut effective_at: Option<&(i64, LabelRange)> = None;
        for entry in &ranges {
            if entry.0 <= at {
                effective_at = Some(entry);
            } else {
                break;
            }
        }
        let preservation_label = match effective_at {
            Some((first, r)) => LabelRange {
                style: r.style,
                prefix: r.prefix.clone(),
                start: r.start.saturating_add(at.saturating_sub(*first)),
            },
            // Default-decimal prefix before the first explicit range: old
            // source page `at` was rendering as decimal `at + 1`.
            None => LabelRange {
                style: LabelStyle::Decimal,
                prefix: String::new(),
                start: at.saturating_add(1),
            },
        };

        let mut result: Vec<(i64, LabelRange)> = ranges
            .into_iter()
            .map(|(idx, range)| {
                if idx >= at {
                    (idx.saturating_add(count), range)
                } else {
                    (idx, range)
                }
            })
            .collect();
        // Insert the preservation range at `at + count` so surviving pages
        // keep their original labels. merge_adjacent_ranges below folds it
        // away when it is redundant with a predecessor.
        result.push((at.saturating_add(count), preservation_label));
        result.sort_by_key(|(idx, _)| *idx);

        // Shifting + preservation may leave neighbours that are structurally
        // equivalent (e.g. the preservation range equals a shifted first
        // range's continuation); fold them away like `remove_pages` does.
        let merged = merge_adjacent_ranges(result);
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
    /// This helper does not know the document's total page count, so
    /// removing pages up to (or past) the end of the labeled range can
    /// still produce a trailing entry describing pages that no longer
    /// exist — e.g. `remove_pages(4, 1)` on a 5-page document writes an
    /// explicit range at output index 4 even though the output has only
    /// pages 0..3. That entry is inert for lookups
    /// ([`Self::label_for_page`] never queries past the caller's page
    /// count) but the on-disk `/PageLabels` tree carries a stale key.
    /// Callers who care about a clean tree at output time should call
    /// [`Self::write_labels`] afterwards with the trimmed range list.
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

        let mut tree = crate::NumberTree::new_empty(self.pdf, true)?;
        let mut cursor = tree.end();
        for (index, value) in entries {
            cursor.insert_after(&mut tree, self.pdf, index, value)?;
        }
        catalog.insert("PageLabels", tree.into_root());
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
    use crate::ObjectRef;
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

    fn pdf_with_catalog_pagelabels(value: Object) -> Pdf<Cursor<Vec<u8>>> {
        let mut pdf = bare_one_page_pdf();
        let catalog_ref = pdf.root_ref().unwrap();
        let mut catalog = pdf
            .resolve_borrowed(catalog_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        catalog.insert("PageLabels", value);
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
    fn ranges_does_not_dirty_a_valid_indirect_tree() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        for object_ref in pdf.dirty_object_refs() {
            pdf.clear_dirty(object_ref);
        }

        let ranges = pdf.page_labels().ranges().expect("read ranges");

        assert_eq!(ranges.len(), 1);
        assert!(pdf.dirty_object_refs().is_empty());
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
    fn get_label_for_page_preserves_qpdf_raw_dictionary_keys() {
        let mut unknown_with_empty_prefix = Dictionary::new();
        unknown_with_empty_prefix.insert("S", Object::Name(b"Z".to_vec()));
        unknown_with_empty_prefix.insert("P", Object::String(Vec::new()));
        unknown_with_empty_prefix.insert("St", Object::Integer(3));

        let mut decimal_without_prefix = Dictionary::new();
        decimal_without_prefix.insert("S", Object::Name(b"D".to_vec()));

        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            Object::Dictionary(unknown_with_empty_prefix),
            Object::Integer(2),
            Object::Dictionary(decimal_without_prefix),
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
        let mut label = Dictionary::new();
        label.insert("S", Object::Name(b"D".to_vec()));
        label.insert("P", Object::String(b"prefix".to_vec()));
        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            Object::Reference(ObjectRef::new(11, 0)),
        ]);
        pdf.set_object(ObjectRef::new(11, 0), Object::Dictionary(label));

        let source = pdf.get_object_handle(ObjectRef::new(11, 0));
        pdf.resolve_object_handle(&source).unwrap();
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
        let mut empty_prefix = Dictionary::new();
        empty_prefix.insert("S", Object::Name(b"D".to_vec()));
        empty_prefix.insert("P", Object::String(Vec::new()));

        let mut decimal = Dictionary::new();
        decimal.insert("S", Object::Name(b"D".to_vec()));
        decimal.insert("St", Object::Integer(10));

        let mut pdf = pdf_with_pagelabels(vec![
            Object::Integer(0),
            label_dict("r", None, None),
            Object::Integer(2),
            Object::Dictionary(empty_prefix),
            Object::Integer(4),
            Object::Dictionary(decimal),
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
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", None, None)]);
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
            Object::Integer(0),
            label_dict("D", None, None),
            Object::Integer(1),
            Object::Integer(99),
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
            Object::Integer(0),
            label_dict("D", Some(1), None),
            Object::Integer(1),
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
            Object::Integer(5),
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
    fn labels_for_selection_handles_missing_tree() {
        let mut pdf = bare_one_page_pdf();
        let labels = pdf.page_labels().labels_for_selection(&[0], 0).unwrap();
        assert_eq!(labels, vec![(0, none_range(1))]);
    }

    #[test]
    fn labels_for_selection_rejects_output_index_overflow() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", None, None)]);
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
            Object::Integer(0),
            label_dict("D", Some(i64::MAX), None),
        ]);
        let error = pdf
            .page_labels()
            .get_label_for_page(1)
            .expect_err("/St offset must use checked arithmetic");
        assert!(error.to_string().contains("offset overflow"));
    }

    #[test]
    fn get_label_for_page_skips_dangling_number_tree_item() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0)]);
        assert!(pdf.page_labels().get_label_for_page(0).unwrap().is_none());
        assert!(pdf
            .repair_diagnostics()
            .entries()
            .iter()
            .any(|warning| warning
                .message
                .contains("items array doesn't have enough elements")));
    }

    #[test]
    fn get_label_for_page_ignores_non_dictionary_values() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), Object::Integer(99)]);
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
        let mut pdf = pdf_with_catalog_pagelabels(Object::Null);
        let mut helper = pdf.page_labels();

        assert!(!helper.has_page_labels().unwrap());
        assert!(helper.get_label_for_page(0).unwrap().is_none());
        assert_eq!(helper.label_string_for_page(0).unwrap(), "1");
        assert!(helper.ranges().unwrap().is_empty());
    }

    #[test]
    fn indirect_null_page_labels_key_is_absent() {
        let null_ref = ObjectRef::new(10, 0);
        let mut pdf = pdf_with_catalog_pagelabels(Object::Reference(null_ref));
        pdf.set_object(null_ref, Object::Null);
        let mut helper = pdf.page_labels();

        assert!(!helper.has_page_labels().unwrap());
        assert!(helper.get_label_for_page(0).unwrap().is_none());
        assert_eq!(helper.label_string_for_page(0).unwrap(), "1");
        assert!(helper.ranges().unwrap().is_empty());
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

    /// PDFDocEncoding-representable prefixes use qpdf's compact encoding while
    /// remaining lossless through a to_dict → from_dict round trip.
    #[test]
    fn to_dict_pdfdoc_prefix_uses_the_compact_qpdf_encoding() {
        let r = LabelRange {
            style: LabelStyle::Decimal,
            prefix: "§".into(),
            start: 1,
        };
        let d = r.to_dict();
        let re_read = LabelRange::from_dict(&d);
        assert_eq!(re_read.prefix, r.prefix, "round-trip must preserve prefix");
        let Object::String(bytes) = d.get("P").expect("/P present") else {
            panic!("/P must be a string"); // cov:ignore: test-shape guard, unreachable given to_dict emits Object::String
        };
        assert_eq!(bytes, &[0xa7]);
    }

    /// ASCII-only prefixes stay verbatim (avoiding a needless UTF-16BE
    /// re-encoding when ASCII is safe under both PDFDocEncoding and UTF-8).
    #[test]
    fn to_dict_ascii_prefix_stays_verbatim() {
        let r = LabelRange {
            style: LabelStyle::Decimal,
            prefix: "App-".into(),
            start: 1,
        };
        let d = r.to_dict();
        let Object::String(bytes) = d.get("P").expect("/P present") else {
            panic!("/P must be a string"); // cov:ignore: test-shape guard, unreachable given to_dict emits Object::String
        };
        assert_eq!(bytes, b"App-", "pure ASCII must stay verbatim");
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
        // A label dict whose /S is not a Name maps to LabelStyle::None via
        // the canonical handle-to-typed compatibility view.
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
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        let mut h = pdf.page_labels();
        let out = h.labels_for_page_range(4, 4, 0).unwrap();
        assert_eq!(out, vec![(0, dec(5))], "St 1 + offset 4 = 5");
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
        assert!(!h.remove_range(0).unwrap());
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

    /// Shorthand for a style-less range (no `/S`), no prefix, starting at
    /// `start` — the shape of qpdf's fabricated "unlabeled page" default.
    fn none_range(start: i64) -> LabelRange {
        LabelRange {
            style: LabelStyle::None,
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

    // ── write_reconstructed_labels / to_reconstructed_dict ────────────────

    #[test]
    fn to_reconstructed_dict_always_includes_st() {
        // Unlike to_dict, /St is present even at the default value 1 — qpdf's
        // getLabelForPage-reconstructed dicts always carry an explicit /St.
        let bare = LabelRange {
            style: LabelStyle::None,
            prefix: String::new(),
            start: 1,
        };
        let dict = bare.to_reconstructed_dict();
        assert_eq!(dict.get("St"), Some(&Object::Integer(1)));
        assert_eq!(dict.get("S"), None, "None style => no /S key");
        assert_eq!(dict.get("P"), None, "empty prefix => no /P key");

        let full = LabelRange {
            style: LabelStyle::Decimal,
            prefix: "A-".into(),
            start: 5,
        };
        let dict2 = full.to_reconstructed_dict();
        assert_eq!(dict2.get("S"), Some(&Object::Name("D".into())));
        assert_eq!(dict2.get("P"), Some(&Object::String(b"A-".to_vec())));
        assert_eq!(dict2.get("St"), Some(&Object::Integer(5)));
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
        let catalog = pdf
            .resolve_borrowed(catalog_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        let Some(Object::Dictionary(page_labels)) = catalog.get("PageLabels") else {
            panic!("/PageLabels must be a direct dictionary, got {catalog:?}"); // cov:ignore: defensive — write_reconstructed_labels always installs a direct dict
        };
        let Some(Object::Array(nums)) = page_labels.get("Nums") else {
            panic!("/Nums must be a direct array"); // cov:ignore: defensive — write_reconstructed_labels always installs a direct array
        };
        assert_eq!(nums.len(), 4, "2 entries * (index, dict)");
        assert_eq!(nums[0], Object::Integer(0));
        assert_eq!(
            nums[1],
            Object::Dictionary({
                let mut d = Dictionary::new();
                d.insert("St", Object::Integer(1));
                d
            }),
            "no /S for a None-style fabricated entry"
        );
        assert_eq!(nums[2], Object::Integer(3));
        assert_eq!(
            nums[3],
            Object::Dictionary({
                let mut d = Dictionary::new();
                d.insert("S", Object::Name("D".into()));
                d.insert("St", Object::Integer(1));
                d
            }),
            "/St 1 stays explicit, unlike to_dict"
        );

        // The high-level reader round-trips the installed entries too.
        let mut h = pdf.page_labels();
        let ranges = h.ranges().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[1].0, 3);
    }

    #[test]
    fn write_reconstructed_labels_replaces_existing_indirect_tree() {
        // A pre-existing indirect /PageLabels root is unconditionally replaced
        // by a fresh direct dict (qpdf never merges).
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("R", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.write_reconstructed_labels(&[(0, none_range(1))]).unwrap();
        }
        let catalog_ref = pdf.root_ref().unwrap();
        let catalog = pdf
            .resolve_borrowed(catalog_ref)
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        assert!(
            matches!(catalog.get("PageLabels"), Some(Object::Dictionary(_))),
            "/PageLabels must now be a direct dict, not the old indirect ref"
        );
    }

    #[test]
    fn write_reconstructed_labels_noop_without_root() {
        // A trailer without /Root must degrade gracefully, matching the same
        // tolerant style as rebuild()/set_range() elsewhere in this file.
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
    }

    #[test]
    fn write_reconstructed_labels_noop_on_non_dict_catalog() {
        let mut pdf = bare_one_page_pdf();
        let catalog_ref = pdf.root_ref().unwrap();
        pdf.set_object(catalog_ref, Object::Integer(0)); // catalog no longer a dict
        let mut h = pdf.page_labels();
        h.write_reconstructed_labels(&[(0, none_range(1))]).unwrap();
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
        // Three ranges after insertion:
        //   0: original roman start 1 (inserted pages 3, 4 render iv, v)
        //   5: preservation roman start 4 (old page 3, now output page 5,
        //      keeps its original "iv" label instead of drifting to "vi")
        //   7: original decimal restart shifted from 5 by count
        assert_eq!(ranges.len(), 3, "got {ranges:?}");
        assert_eq!(ranges[0].0, 0);
        assert_eq!(ranges[1].0, 5);
        assert_eq!(ranges[1].1.start, 4, "old source page 3's roman position");
        assert_eq!(ranges[2].0, 7);
        // End-to-end: old page 3's label survives at its new position.
        assert_eq!(h.label_string_for_page(5).unwrap(), "iv");
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

    /// Cover the `None` arm of insert_pages's preservation-label match:
    /// when `at` sits BEFORE the first explicit range, no entry has
    /// `entry.0 <= at`, so `effective_at` stays None and the fabricated
    /// LabelStyle::Decimal-at-1 range at `at + count` runs.
    #[test]
    fn insert_pages_before_first_range_fabricates_decimal_preservation() {
        // First explicit range at index 5 (roman), leaving pages 0..4 with
        // the PDF default decimal sequence "1".."5".
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(5), label_dict("r", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            // Insert 2 pages at position 0 — inside the default prefix, no
            // explicit range covers `at = 0`.
            h.insert_pages(0, 2).unwrap();
        }
        let mut h = pdf.page_labels();
        // Old page 0 (previously "1") moves to output index 2 and must
        // still render as "1"; the fabricated Decimal-start-1 preservation
        // range at index 2 encodes that.
        assert_eq!(h.label_string_for_page(2).unwrap(), "1");
        // Old page 4 (previously "5") moves to output index 6, still "5".
        assert_eq!(h.label_string_for_page(6).unwrap(), "5");
        // Roman range shifted from 5 to 7.
        assert_eq!(h.label_string_for_page(7).unwrap(), "i");
    }

    #[test]
    fn insert_pages_preserves_old_page_labels_across_gap_close() {
        // (5, Decimal, start 8) is an intentional forward jump over
        // (0, Decimal, start 1) — numbers 6 and 7 are deliberately skipped.
        // Insert 2 pages at position 2. The pre-fix version dropped the
        // preservation range and merged everything into a single (0, dec 1)
        // sequence, breaking old page 2's original "3" → it showed "5"
        // instead after insertion. The correct behaviour keeps old pages'
        // labels stable: old page 2 stays "3" at its new position 4.
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
        // Old page 2 (label "3") now sits at output page 4 and must still
        // render as "3", not drift to "5".
        assert_eq!(h.label_string_for_page(4).unwrap(), "3");
        // Old page 5 (the deliberate restart to "8") now sits at output
        // page 7 and must still render as "8".
        assert_eq!(h.label_string_for_page(7).unwrap(), "8");
    }

    #[test]
    fn insert_pages_after_last_range_leaves_entries_unchanged() {
        let mut pdf = pdf_with_pagelabels(vec![Object::Integer(0), label_dict("D", Some(1), None)]);
        {
            let mut h = pdf.page_labels();
            h.insert_pages(10, 3).unwrap(); // append pages well past the only range
        }
        let mut h = pdf.page_labels();
        // Inserted pages 10-12 continue the decimal-1 sequence (labels
        // "11","12","13"); old page 10 moves to position 13 but its label
        // was already "11" under the (0, dec 1) range, so it must still
        // render as "11" — the preservation range at index 13 with start
        // 11 encodes this invariant even though it looks redundant to a
        // naïve reader.
        assert_eq!(h.label_string_for_page(13).unwrap(), "11");
        // Pages before the insertion point are untouched.
        assert_eq!(h.label_string_for_page(0).unwrap(), "1");
        assert_eq!(h.label_string_for_page(9).unwrap(), "10");
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
