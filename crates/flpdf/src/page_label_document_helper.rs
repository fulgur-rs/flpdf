//! qpdf `QPDFPageLabelDocumentHelper`-equivalent page-label access.
//!
//! [`PageLabelDocumentHelper`] reads, renders (ISO 32000-1 §12.4.2), and edits
//! the catalog `/PageLabels` number tree. [`LabelRange`] models one label range
//! (`/S` style, `/P` prefix, `/St` start). The number-tree walking/building is
//! delegated to [`crate::name_number_tree`].

use crate::name_number_tree::DEFAULT_MAX_TREE_DEPTH;
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
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
    /// [`crate::json_inspect::decode_pdf_text_string`] with lossy fallback.
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

/// Format `value` as a roman numeral (`upper` → uppercase). Empty for `value <= 0`.
fn to_roman(value: i64, upper: bool) -> String {
    if value <= 0 {
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
/// Empty for `value <= 0`.
fn to_alpha(value: i64, upper: bool) -> String {
    if value <= 0 {
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
    pub fn has_page_labels(&mut self) -> Result<bool> {
        Ok(self.pagelabels_root()?.is_some())
    }

    /// All label ranges as `(first_page_index, LabelRange)`, ascending by index.
    /// Empty when `/PageLabels` is absent.
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
                    Object::Reference(r) => pdf.resolve_borrowed(r)?.as_dict().cloned(),
                    _ => None,
                };
                Ok(dict.map(|d| LabelRange::from_dict(&d)))
            },
            DEFAULT_MAX_TREE_DEPTH,
        )
    }

    /// The effective label for a 0-based page index (qpdf `getLabelForPage`):
    /// the range whose first index is the largest `<= page_idx`, with `start`
    /// offset to that page. `None` when no range applies (no `/PageLabels`, or
    /// the page precedes the first range).
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
            let offset = page_idx - first;
            LabelRange {
                style: r.style,
                prefix: r.prefix.clone(),
                start: r.start + offset,
            }
        }))
    }

    /// The rendered display string for a 0-based page index. Falls back to
    /// 1-based decimal (`(page_idx + 1)`) when no range applies — matching the
    /// "default 1-based numeric labels" requirement.
    pub fn label_string_for_page(&mut self, page_idx: i64) -> Result<String> {
        match self.label_for_page(page_idx)? {
            Some(effective) => Ok(effective.format(effective.start)),
            None => Ok((page_idx + 1).to_string()),
        }
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

    fn pdf_with_pagelabels(nums: Vec<Object>) -> Pdf<Cursor<Vec<u8>>> {
        // Minimal one-page PDF; then attach an inline /PageLabels leaf via set_object.
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
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open");
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
        // Remove the (empty) /PageLabels to exercise the absent path too:
        let mut h = pdf.page_labels();
        assert_eq!(h.label_string_for_page(0).unwrap(), "1");
        assert_eq!(h.label_string_for_page(4).unwrap(), "5");
        assert!(h.label_for_page(0).unwrap().is_none());
    }

    #[test]
    fn page_before_first_range_defaults_to_decimal() {
        let mut pdf =
            pdf_with_pagelabels(vec![Object::Integer(3), label_dict("R", Some(1), None)]);
        let mut h = pdf.page_labels();
        assert_eq!(
            h.label_string_for_page(0).unwrap(),
            "1",
            "page before first range"
        );
        assert_eq!(h.label_string_for_page(3).unwrap(), "I");
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
        assert_eq!(none.format(9), "Cover", "None style => prefix only, no number");
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
}
