//! Parser for qpdf's `--rotate` flag specification.
//!
//! # Syntax
//!
//! ```text
//! rotate-spec  ::= ['+' | '-'] angle [':' page-range]
//! angle        ::= '0' | '90' | '180' | '270'
//! page-range   ::= (qpdf page-range syntax, see [`PageRange`])
//! ```
//!
//! ## Sign semantics
//!
//! NOTE: This implementation intentionally diverges from one interpretation of
//! qpdf where "no sign = Assign". Per the flpdf-9hc.8.5 specification:
//!
//! - `+angle` or `angle` (no sign) → additive rotation (`RotateMode::Add`, positive degrees)
//! - `-angle`                       → additive rotation (`RotateMode::Add`, **negative** degrees)
//!
//! `RotateMode::Assign` is not used here; it is reserved for a future issue.
//! The additive sign-encoded form is the natural representation because
//! `compose_rotate` accepts signed `degrees` and `normalize_rotate` handles
//! negatives correctly (e.g. `Add(-90)` on existing=0 gives 270).
//!
//! ## Page-range
//!
//! If no `:` is present the spec applies to all pages (equivalent to an empty
//! page-range string in [`PageRange`]).  A `:` must be followed by a non-empty
//! page-range string; a trailing `:` with nothing after it is an error.
//!
//! ## Multiple `--rotate` flags
//!
//! The parser returns a single [`RotateSpec`].  Callers that accept multiple
//! `--rotate` flags should collect them into a `Vec<RotateSpec>`; the specs are
//! applied in order (responsibility of the CLI layer, flpdf-9hc.8.12).

use crate::page_range::PageRange;
use crate::page_rotate::{RotateMode, RotateOp};
use crate::{Error, Result};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A parsed `--rotate` specification: a rotation operation plus an optional
/// page-range selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotateSpec {
    /// The rotation operation (mode + degrees).  `mode` is always
    /// [`RotateMode::Add`] in this parser; `degrees` may be negative to
    /// represent counter-clockwise rotation.
    pub op: RotateOp,
    /// The page range to which the rotation applies.  A [`PageRange`] parsed
    /// from an empty string means "all pages".
    pub range: PageRange,
}

impl RotateSpec {
    /// Parse a single `--rotate` argument string.
    ///
    /// # Accepted forms
    ///
    /// | Input       | `op.degrees` | `op.mode`       | `range`     |
    /// |-------------|-------------|-----------------|-------------|
    /// | `90`        | `90`        | `Add`           | all pages   |
    /// | `+90`       | `90`        | `Add`           | all pages   |
    /// | `-90`       | `-90`       | `Add`           | all pages   |
    /// | `+90:1-3`   | `90`        | `Add`           | pages 1-3   |
    /// | `-90:5`     | `-90`       | `Add`           | page 5      |
    /// | `180:r1`    | `180`       | `Add`           | last page   |
    ///
    /// # Errors
    ///
    /// - Empty input.
    /// - Invalid or absent angle digits.
    /// - Angle not in `{0, 90, 180, 270}`.
    /// - `:` present but nothing follows it.
    /// - Malformed page-range syntax (forwarded from [`PageRange::parse`]).
    pub fn parse(input: &str) -> Result<Self> {
        if input.is_empty() {
            return Err(Error::parse(
                0,
                "rotate spec is empty; expected [+|-]angle[:page-range]",
            ));
        }

        let mut pos = 0usize;
        let bytes = input.as_bytes();

        // ----------------------------------------------------------------
        // 1. Optional sign.
        // ----------------------------------------------------------------
        let negative = match bytes.get(pos) {
            Some(b'+') => {
                pos += 1;
                false
            }
            Some(b'-') => {
                pos += 1;
                true
            }
            _ => false,
        };

        // After a sign there must be digits.
        if pos >= bytes.len() {
            return Err(Error::parse(
                pos,
                "expected angle digits after sign; valid angles are 0, 90, 180, 270",
            ));
        }

        // ----------------------------------------------------------------
        // 2. Parse angle digits (up to the ':' or end of string).
        // ----------------------------------------------------------------
        let angle_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }

        if pos == angle_start {
            return Err(Error::parse(
                angle_start,
                format!(
                    "expected angle at position {angle_start}; valid angles are 0, 90, 180, 270"
                ),
            ));
        }

        let angle_str = &input[angle_start..pos];
        let angle_value: u32 = angle_str.parse().map_err(|_| {
            Error::parse(
                angle_start,
                format!("angle '{angle_str}' is too large; valid angles are 0, 90, 180, 270"),
            )
        })?;

        // Validate angle is one of the qpdf-allowed values.
        if !matches!(angle_value, 0 | 90 | 180 | 270) {
            return Err(Error::parse(
                angle_start,
                format!("angle {angle_value} is not allowed; qpdf accepts only 0, 90, 180, or 270"),
            ));
        }

        let degrees: i32 = if negative {
            // NOTE: negative sign encodes counter-clockwise (or CW depending on
            // convention) additive rotation.  We store it as negative i32 degrees.
            -(angle_value as i32)
        } else {
            angle_value as i32
        };

        // ----------------------------------------------------------------
        // 3. Optional ':' + page-range.
        // ----------------------------------------------------------------
        let range = if pos < bytes.len() {
            // There must be a ':' here; anything else is invalid.
            if bytes[pos] != b':' {
                let unexpected = input[pos..].chars().next().unwrap_or('?');
                return Err(Error::parse(
                    pos,
                    format!(
                        "unexpected character '{unexpected}' at position {pos}; expected ':' or end of input"
                    ),
                ));
            }
            pos += 1; // consume ':'

            // The page-range after ':' must not be empty.
            if pos >= bytes.len() {
                return Err(Error::parse(
                    pos,
                    "expected a page-range after ':'; got end of input",
                ));
            }

            let range_str = &input[pos..];
            PageRange::parse(range_str).map_err(|e| {
                // Re-wrap with offset adjusted to the page-range substring.
                match e {
                    Error::Parse { offset, message } => Error::parse(pos + offset, message),
                    other => other,
                }
            })?
        } else {
            // No ':' → all pages.
            PageRange::parse("").expect("empty string always yields all-pages sentinel")
        };

        Ok(RotateSpec {
            op: RotateOp {
                mode: RotateMode::Add,
                degrees,
            },
            range,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_range::{Endpoint, PageRangeEntry};
    use crate::page_rotate::compose_rotate;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn parse_ok(input: &str) -> RotateSpec {
        RotateSpec::parse(input).unwrap_or_else(|e| panic!("expected Ok for {input:?}, got: {e}"))
    }

    fn parse_err(input: &str) -> String {
        RotateSpec::parse(input)
            .err()
            .unwrap_or_else(|| panic!("expected Err for {input:?}"))
            .to_string()
    }

    // -----------------------------------------------------------------------
    // qpdf documented examples
    // -----------------------------------------------------------------------

    #[test]
    fn plus_90_colon_1_3() {
        // "+90:1-3" → Add(+90), pages 1-3
        let spec = parse_ok("+90:1-3");
        assert_eq!(spec.op.mode, RotateMode::Add);
        assert_eq!(spec.op.degrees, 90);
        let pages = spec.range.resolve(10).unwrap();
        assert_eq!(pages, vec![1, 2, 3]);
    }

    #[test]
    fn minus_90_colon_5() {
        // "-90:5" → Add(-90), page 5
        let spec = parse_ok("-90:5");
        assert_eq!(spec.op.mode, RotateMode::Add);
        assert_eq!(spec.op.degrees, -90);
        let pages = spec.range.resolve(10).unwrap();
        assert_eq!(pages, vec![5]);
    }

    #[test]
    fn bare_180_colon_r1() {
        // "180:r1" → Add(+180), last page
        let spec = parse_ok("180:r1");
        assert_eq!(spec.op.mode, RotateMode::Add);
        assert_eq!(spec.op.degrees, 180);
        let entries = spec.range.entries.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].start, Endpoint::FromEnd(1));
    }

    #[test]
    fn bare_90_no_range_means_all_pages() {
        // "90" (no colon) → Add(+90), all pages
        let spec = parse_ok("90");
        assert_eq!(spec.op.mode, RotateMode::Add);
        assert_eq!(spec.op.degrees, 90);
        assert!(spec.range.entries.is_none(), "expected all-pages sentinel");
        let pages = spec.range.resolve(3).unwrap();
        assert_eq!(pages, vec![1, 2, 3]);
    }

    #[test]
    fn zero_rotation_valid() {
        // "0:1" is a no-op rotation on page 1 — syntactically valid
        let spec = parse_ok("0:1");
        assert_eq!(spec.op.degrees, 0);
        let pages = spec.range.resolve(5).unwrap();
        assert_eq!(pages, vec![1]);
    }

    #[test]
    fn minus_180_colon_z() {
        // "-180:z" → Add(-180), last page
        let spec = parse_ok("-180:z");
        assert_eq!(spec.op.degrees, -180);
        let entries = spec.range.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::Z);
    }

    #[test]
    fn plus_270_no_range() {
        // "+270" → Add(+270), all pages
        let spec = parse_ok("+270");
        assert_eq!(spec.op.degrees, 270);
        assert!(spec.range.entries.is_none());
    }

    // -----------------------------------------------------------------------
    // Sign / no-sign variants of the same angle
    // -----------------------------------------------------------------------

    #[test]
    fn positive_sign_explicit() {
        let spec = parse_ok("+90");
        assert_eq!(spec.op.degrees, 90);
        assert_eq!(spec.op.mode, RotateMode::Add);
    }

    #[test]
    fn negative_sign() {
        let spec = parse_ok("-90");
        assert_eq!(spec.op.degrees, -90);
        assert_eq!(spec.op.mode, RotateMode::Add);
    }

    #[test]
    fn no_sign_treated_as_add() {
        // NOTE: no-sign is Add (not Assign) per flpdf-9hc.8.5 specification.
        let spec = parse_ok("270");
        assert_eq!(spec.op.mode, RotateMode::Add);
        assert_eq!(spec.op.degrees, 270);
    }

    // -----------------------------------------------------------------------
    // Compose sanity: negative degrees produce correct final values
    // -----------------------------------------------------------------------

    #[test]
    fn compose_negative_90_from_zero_gives_270() {
        // existing=0, op=Add(-90) → 270
        let spec = parse_ok("-90");
        assert_eq!(compose_rotate(0, &spec.op), 270);
    }

    #[test]
    fn compose_plus_90_from_270_wraps_to_zero() {
        let spec = parse_ok("+90");
        assert_eq!(compose_rotate(270, &spec.op), 0);
    }

    // -----------------------------------------------------------------------
    // Invalid angle values
    // -----------------------------------------------------------------------

    #[test]
    fn angle_45_is_invalid() {
        let msg = parse_err("45:1");
        assert!(
            msg.contains("45") && (msg.contains("not allowed") || msg.contains("0, 90, 180, 270")),
            "got: {msg}"
        );
    }

    #[test]
    fn angle_91_is_invalid() {
        let msg = parse_err("91:1");
        assert!(
            msg.contains("91") && (msg.contains("not allowed") || msg.contains("0, 90, 180, 270")),
            "got: {msg}"
        );
    }

    #[test]
    fn angle_360_is_invalid() {
        let msg = parse_err("360:1");
        assert!(
            msg.contains("360") && (msg.contains("not allowed") || msg.contains("0, 90, 180, 270")),
            "got: {msg}"
        );
    }

    #[test]
    fn negative_45_is_invalid() {
        let msg = parse_err("-45:1");
        assert!(
            msg.contains("45") && (msg.contains("not allowed") || msg.contains("0, 90, 180, 270")),
            "got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Structural errors
    // -----------------------------------------------------------------------

    #[test]
    fn empty_input_is_invalid() {
        let msg = parse_err("");
        assert!(!msg.is_empty(), "got: {msg}");
    }

    #[test]
    fn bare_sign_only_is_invalid() {
        let msg = parse_err("+");
        assert!(
            msg.contains("expected angle") || msg.contains("digits"),
            "got: {msg}"
        );
    }

    #[test]
    fn bare_minus_only_is_invalid() {
        let msg = parse_err("-");
        assert!(
            msg.contains("expected angle") || msg.contains("digits"),
            "got: {msg}"
        );
    }

    #[test]
    fn non_numeric_is_invalid() {
        let msg = parse_err("abc");
        assert!(
            msg.contains("expected angle") || msg.contains("angle at position"),
            "got: {msg}"
        );
    }

    #[test]
    fn trailing_colon_no_range_is_invalid() {
        // "90:" — colon present but nothing follows
        let msg = parse_err("90:");
        assert!(
            msg.contains("expected a page-range after ':'") || msg.contains("end of input"),
            "got: {msg}"
        );
    }

    #[test]
    fn colon_without_angle_is_invalid() {
        // ":1-3" — no angle before colon
        let msg = parse_err(":1-3");
        assert!(!msg.is_empty(), "got: {msg}");
    }

    #[test]
    fn double_sign_is_invalid() {
        // "+-90:1" — two signs
        let msg = parse_err("+-90:1");
        // After consuming '+', the next char is '-' which is not a digit;
        // angle parsing fails.
        assert!(!msg.is_empty(), "got: {msg}");
    }

    #[test]
    fn unexpected_char_after_angle_is_invalid() {
        // "90x" — 'x' is unexpected
        let msg = parse_err("90x");
        assert!(
            msg.contains("unexpected character") || msg.contains("'x'"),
            "got: {msg}"
        );
    }

    // -----------------------------------------------------------------------
    // Multiple-spec order preservation (Vec accumulation pattern)
    // -----------------------------------------------------------------------

    #[test]
    fn multiple_specs_parsed_in_order() {
        // Callers collect Vec<RotateSpec> in CLI order.
        let specs: Vec<RotateSpec> = ["+90:1-3", "-90:5", "180:r1"]
            .iter()
            .map(|s| RotateSpec::parse(s).unwrap())
            .collect();

        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].op.degrees, 90);
        assert_eq!(specs[1].op.degrees, -90);
        assert_eq!(specs[2].op.degrees, 180);
    }

    // -----------------------------------------------------------------------
    // Page-range reuse: verify PageRange entries are forwarded correctly
    // -----------------------------------------------------------------------

    #[test]
    fn range_1_3_has_correct_entries() {
        let spec = parse_ok("+90:1-3");
        let entries: &[PageRangeEntry] = spec.range.entries.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].start, Endpoint::Num(1));
        assert_eq!(entries[0].end, Some(Endpoint::Num(3)));
    }

    #[test]
    fn range_r1_has_from_end_entry() {
        let spec = parse_ok("180:r1");
        let entries = spec.range.entries.as_ref().unwrap();
        assert_eq!(entries[0].start, Endpoint::FromEnd(1));
        assert!(entries[0].end.is_none());
    }

    #[test]
    fn malformed_page_range_produces_error() {
        // "90:0" — page 0 is invalid in PageRange
        let msg = parse_err("90:0");
        assert!(
            msg.contains("0 is invalid") || msg.contains("page number 0"),
            "got: {msg}"
        );
    }
}
