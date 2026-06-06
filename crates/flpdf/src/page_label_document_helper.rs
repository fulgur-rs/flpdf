//! qpdf `QPDFPageLabelDocumentHelper`-equivalent page-label access.
//!
//! [`PageLabelDocumentHelper`] reads, renders (ISO 32000-1 §12.4.2), and edits
//! the catalog `/PageLabels` number tree. [`LabelRange`] models one label range
//! (`/S` style, `/P` prefix, `/St` start). The number-tree walking/building is
//! delegated to [`crate::name_number_tree`].

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

#[cfg(test)]
mod tests {
    use super::*;

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
