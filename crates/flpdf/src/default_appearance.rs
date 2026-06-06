//! Parser for the PDF `/DA` (default appearance) string.
//!
//! A `/DA` value is a small content-stream fragment such as
//! `/Helv 0 Tf 0 g` that sets the font, size, and colour used to render
//! text in a form field.  This module tokenises the fragment with the
//! shared [`crate::content_stream::ContentStreamParser`] and extracts the
//! structured [`DefaultAppearance`] value.
//!
//! # Inheritance
//!
//! Call [`parse_default_appearance`] with **already-inherited** `/DA` bytes.
//! Inheritance resolution (falling back to the `/AcroForm` root `/DA` when a
//! field-level `/DA` is absent) is the caller's responsibility.

use crate::content_stream::{ContentStreamParser, ContentToken};
use crate::Object;

// ── Public types ─────────────────────────────────────────────────────────────

/// Colour model for text rendering in a form field.
///
/// Corresponds to the `g`, `rg`, and `k` PDF colour operators.
/// The default (when the `/DA` string contains no colour operator) is
/// [`TextColor::Gray`]`(0.0)` (opaque black).
#[derive(Debug, Clone, PartialEq)]
pub enum TextColor {
    /// Greyscale: 0.0 = black, 1.0 = white.
    Gray(f64),
    /// Device RGB: (red, green, blue), each in `[0.0, 1.0]`.
    Rgb(f64, f64, f64),
    /// Device CMYK: (cyan, magenta, yellow, black), each in `[0.0, 1.0]`.
    Cmyk(f64, f64, f64, f64),
}

/// Structured representation of a parsed `/DA` (default appearance) string.
///
/// Pass **already-inherited** `/DA` bytes to [`parse_default_appearance`].
/// When a field-level `/DA` is absent the caller should fall back to the
/// `/AcroForm` root `/DA` before calling this function.
#[derive(Debug, Clone, PartialEq)]
pub struct DefaultAppearance {
    /// Font resource name, without the leading `/` (e.g. `b"Helv"`).
    ///
    /// `None` when the `/DA` string contains no `Tf` operator or the operands
    /// are malformed.  The caller should substitute a default font.
    pub font_name: Option<Vec<u8>>,

    /// Font size in points.  `0.0` when the `Tf` size operand is zero (PDF
    /// convention for "auto-size"); check [`auto_size`](Self::auto_size)
    /// instead of comparing this field to `0.0` directly.
    pub font_size: f64,

    /// `true` when `font_size == 0.0`, indicating that the viewer should
    /// choose the size automatically to fit the field bounds.
    pub auto_size: bool,

    /// Text colour.  Defaults to [`TextColor::Gray`]`(0.0)` (black).
    pub color: TextColor,
}

// ── Public function ──────────────────────────────────────────────────────────

/// Parse a `/DA` content-stream fragment into a [`DefaultAppearance`].
///
/// The argument should be the **already-inherited** `/DA` bytes (field-level
/// `/DA` if present, otherwise the `/AcroForm` root `/DA`).
///
/// Malformed or unrecognised tokens are silently skipped.  When an operator
/// appears more than once the **last** occurrence wins.
///
/// # Examples
///
/// ```
/// use flpdf::{parse_default_appearance, TextColor};
///
/// let da = parse_default_appearance(b"/Helv 0 Tf 0 g");
/// assert_eq!(da.font_name.as_deref(), Some(b"Helv" as &[u8]));
/// assert!(da.auto_size);
/// assert_eq!(da.color, TextColor::Gray(0.0));
/// ```
pub fn parse_default_appearance(da: &[u8]) -> DefaultAppearance {
    let mut font_name: Option<Vec<u8>> = None;
    let mut font_size: f64 = 0.0;
    let mut auto_size: bool = true;
    let mut color: TextColor = TextColor::Gray(0.0);

    for token in ContentStreamParser::new(da).flatten() {
        let ContentToken::Op { operands, operator } = token else {
            continue;
        };

        match operator.as_slice() {
            b"Tf" => {
                // Operands: /FontName size. PDF operators consume their operands
                // from the top of the stack, so read the **last** two operands
                // (`[.., name, size]`) rather than the first two — any leading
                // dangling operands (from malformed runs) must not be mistaken
                // for `Tf`'s arguments. `Tf` is only meaningful when both a name
                // and a numeric size are present, so update all three fields
                // together; a malformed `Tf` (missing/typed operand) is ignored
                // wholesale rather than partially applied.
                if let [.., name, size] = operands.as_slice() {
                    if let (Some(name), Some(size)) = (name.as_name(), obj_as_f64(size)) {
                        font_name = Some(name.to_vec());
                        font_size = size;
                        auto_size = size == 0.0;
                    }
                }
            }
            b"g" => {
                // Greyscale: g  (1 operand — top of stack)
                if let Some(gray) = operands.last().and_then(obj_as_f64) {
                    color = TextColor::Gray(gray);
                }
            }
            b"rg" => {
                // Device RGB: r g b  (3 operands — top of stack)
                if let [.., r, g, b] = operands.as_slice() {
                    if let (Some(r), Some(g), Some(b)) =
                        (obj_as_f64(r), obj_as_f64(g), obj_as_f64(b))
                    {
                        color = TextColor::Rgb(r, g, b);
                    }
                }
            }
            b"k" => {
                // Device CMYK: c m y k  (4 operands — top of stack)
                if let [.., c, m, y, k] = operands.as_slice() {
                    if let (Some(c), Some(m), Some(y), Some(k)) =
                        (obj_as_f64(c), obj_as_f64(m), obj_as_f64(y), obj_as_f64(k))
                    {
                        color = TextColor::Cmyk(c, m, y, k);
                    }
                }
            }
            _ => {}
        }
    }

    DefaultAppearance {
        font_name,
        font_size,
        auto_size,
        color,
    }
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Convert a PDF [`Object`] (Integer or Real) to `f64`.
///
/// Returns `None` for any other object type, including indirect references
/// (which do not appear in content-stream fragments).
fn obj_as_f64(obj: &Object) -> Option<f64> {
    obj.as_real().or_else(|| obj.as_integer().map(|i| i as f64))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: compare two f64 values within a small epsilon.
    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn helv_auto_size_black() {
        // '/Helv 0 Tf 0 g' — canonical auto-size black text
        let da = parse_default_appearance(b"/Helv 0 Tf 0 g");
        assert_eq!(da.font_name.as_deref(), Some(b"Helv" as &[u8]));
        assert!(approx_eq(da.font_size, 0.0));
        assert!(da.auto_size);
        assert_eq!(da.color, TextColor::Gray(0.0));
    }

    #[test]
    fn helv_size12_rgb_red() {
        // '/Helv 12 Tf 1 0 0 rg' — 12pt, red
        let da = parse_default_appearance(b"/Helv 12 Tf 1 0 0 rg");
        assert_eq!(da.font_name.as_deref(), Some(b"Helv" as &[u8]));
        assert!(approx_eq(da.font_size, 12.0));
        assert!(!da.auto_size);
        assert_eq!(da.color, TextColor::Rgb(1.0, 0.0, 0.0));
    }

    #[test]
    fn courier_cmyk_black() {
        // '/Cour 10 Tf 0 0 0 1 k' — 10pt, CMYK black
        let da = parse_default_appearance(b"/Cour 10 Tf 0 0 0 1 k");
        assert_eq!(da.font_name.as_deref(), Some(b"Cour" as &[u8]));
        assert!(approx_eq(da.font_size, 10.0));
        assert!(!da.auto_size);
        assert_eq!(da.color, TextColor::Cmyk(0.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn empty_input_returns_defaults() {
        let da = parse_default_appearance(b"");
        assert_eq!(da.font_name, None);
        assert!(approx_eq(da.font_size, 0.0));
        assert!(da.auto_size);
        assert_eq!(da.color, TextColor::Gray(0.0));
    }

    #[test]
    fn malformed_tokens_fallback_to_defaults() {
        // Garbage bytes that the parser cannot make into a valid Tf/g/rg/k
        let da = parse_default_appearance(b"not valid pdf tokens !@#");
        assert_eq!(da.font_name, None);
        assert!(approx_eq(da.font_size, 0.0));
        assert!(da.auto_size);
        assert_eq!(da.color, TextColor::Gray(0.0));
    }

    #[test]
    fn real_font_size() {
        // '/F1 11.5 Tf' — Real operand for size
        let da = parse_default_appearance(b"/F1 11.5 Tf");
        assert_eq!(da.font_name.as_deref(), Some(b"F1" as &[u8]));
        assert!(approx_eq(da.font_size, 11.5));
        assert!(!da.auto_size);
        // No colour operator → default
        assert_eq!(da.color, TextColor::Gray(0.0));
    }

    #[test]
    fn last_color_wins() {
        // Two colour operators — last one (k) should win
        let da = parse_default_appearance(b"/Helv 12 Tf 1 0 0 rg 0 0 0 1 k");
        assert_eq!(da.color, TextColor::Cmyk(0.0, 0.0, 0.0, 1.0));
    }

    #[test]
    fn last_tf_wins() {
        // Two Tf operators — last one should win
        let da = parse_default_appearance(b"/Helv 12 Tf /Cour 9 Tf");
        assert_eq!(da.font_name.as_deref(), Some(b"Cour" as &[u8]));
        assert!(approx_eq(da.font_size, 9.0));
    }

    #[test]
    fn grey_color_operator() {
        // Explicit greyscale value
        let da = parse_default_appearance(b"/Helv 10 Tf 0.5 g");
        assert_eq!(da.color, TextColor::Gray(0.5));
    }

    #[test]
    fn missing_tf_operands_skipped() {
        // Tf with no operands → font_name stays None, auto_size stays true
        let da = parse_default_appearance(b"Tf 0 g");
        assert_eq!(da.font_name, None);
        assert!(da.auto_size);
        assert_eq!(da.color, TextColor::Gray(0.0));
    }

    #[test]
    fn malformed_trailing_tf_does_not_partially_overwrite() {
        // A valid `Tf` followed by a malformed one (`/Bad Tf`, missing the
        // numeric size) must be ignored wholesale: the earlier valid pair is
        // preserved rather than producing the never-present (Bad, 12) combo.
        let da = parse_default_appearance(b"/Helv 12 Tf /Bad Tf");
        assert_eq!(da.font_name.as_deref(), Some(b"Helv" as &[u8]));
        assert!(approx_eq(da.font_size, 12.0));
        assert!(!da.auto_size);
    }

    #[test]
    fn leading_dangling_operands_use_stack_top() {
        // PDF operators consume operands from the top of the stack. When extra
        // leading operands precede an operator (malformed run, unrecognised
        // sequence), the operator's real arguments are the *last* ones. Reading
        // from the front would mistake the dangling `99 88` / `7 7` for the
        // operands and either skip or mis-parse the operator.
        let da = parse_default_appearance(b"99 88 /Helv 12 Tf 7 7 0.25 g");
        assert_eq!(da.font_name.as_deref(), Some(b"Helv" as &[u8]));
        assert!(approx_eq(da.font_size, 12.0));
        assert!(!da.auto_size);
        assert_eq!(da.color, TextColor::Gray(0.25));
    }

    #[test]
    fn leading_dangling_operands_rgb_and_cmyk_use_stack_top() {
        // rg / k must also read their 3 / 4 operands from the top of the stack.
        let rgb = parse_default_appearance(b"42 1.0 0.0 0.0 rg");
        assert_eq!(rgb.color, TextColor::Rgb(1.0, 0.0, 0.0));

        let cmyk = parse_default_appearance(b"9 9 0.1 0.2 0.3 0.4 k");
        assert_eq!(cmyk.color, TextColor::Cmyk(0.1, 0.2, 0.3, 0.4));
    }
}
