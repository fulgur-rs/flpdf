//! Parse and format PDF version identifiers.
//!
//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.
//!
//! Public API: qpdf 11.9.0 include/qpdf/PDFVersion.hh.

use crate::qutil::{qpdf_string_to_int_checked, QpdfIntParse};
use crate::Error;

/// A PDF major/minor version paired with an optional extension level.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PdfVersion {
    major: i32,
    minor: i32,
    extension_level: i64,
}

impl PdfVersion {
    /// Creates a PDF version value.
    ///
    /// qpdf's `PDFVersion` stores `major_version`/`minor_version` as `int`
    /// (`include/qpdf/PDFVersion.hh:60-62`), not a narrower type.
    pub const fn new(major: i32, minor: i32, extension_level: i64) -> Self {
        Self {
            major,
            minor,
            extension_level,
        }
    }

    /// Parses the existing flpdf `M.m` version syntax with extension level 0.
    ///
    /// This has no qpdf counterpart (unlike the crate-internal digit-run
    /// prefix parser mirroring `QPDF::getVersionAsPDFVersion`'s regex and
    /// i32 range), so it keeps its pre-existing `u8`-bounded numeric range
    /// deliberately: a major or minor run of 3+ digits still parses to
    /// `None`, matching this function's own prior behavior rather than
    /// widening as a side effect of [`PdfVersion`]'s field type.
    pub fn parse(value: &str) -> Option<Self> {
        let (major, minor) = value.split_once('.')?;
        Some(Self::new(
            i32::from(major.parse::<u8>().ok()?),
            i32::from(minor.parse::<u8>().ok()?),
            0,
        ))
    }

    /// Replaces this value when `other` is greater.
    pub fn update_if_greater(&mut self, other: Self) {
        if *self < other {
            *self = other;
        }
    }

    /// Returns the `M.m` version string and extension level.
    pub fn get_version(self) -> (String, i64) {
        (
            format!("{}.{}", self.major, self.minor),
            self.extension_level,
        )
    }

    /// Returns the major version.
    pub const fn major(self) -> i32 {
        self.major
    }

    /// Returns the minor version.
    pub const fn minor(self) -> i32 {
        self.minor
    }

    /// Returns the extension level.
    pub const fn extension_level(self) -> i64 {
        self.extension_level
    }

    pub(crate) const fn static_version_str(self) -> Option<&'static str> {
        match (self.major, self.minor) {
            (1, 3) => Some("1.3"),
            (1, 4) => Some("1.4"),
            (1, 5) => Some("1.5"),
            (1, 6) => Some("1.6"),
            (1, 7) => Some("1.7"),
            _ => None,
        }
    }
}

/// Parses a PDF version string of the form `M.m`.
pub fn parse_pdf_version(value: &str) -> Option<PdfVersion> {
    PdfVersion::parse(value)
}

/// The leading `M.m` digit-run prefix of a version header string, matching
/// `QPDF::getVersionAsPDFVersion`'s regex
/// (`^[[:space:]]*([0-9]+)\.([0-9]+)`, `libqpdf/QPDF.cc:2305-2320`): skip
/// leading whitespace, take a digit run, a literal `.`, and a following
/// digit run. Falls back to `Ok((1, 3))` when the string does not start that
/// way (the regex simply does not match). A captured digit run that
/// overflows `QUtil::string_to_int`'s i32 range instead becomes `Err`: qpdf
/// calls `QUtil::string_to_int` uncaught here (`QPDF.cc:2314,2317`), so the
/// `std::range_error` it throws propagates out of `getVersionAsPDFVersion`
/// rather than falling back to a default.
///
/// # Errors
///
/// Returns [`Error::System`] when a captured digit run overflows i32,
/// carrying qpdf's own `integer out of range converting ...` text.
pub(crate) fn leading_major_minor(value: &str) -> Result<(i32, i32), Error> {
    const FALLBACK: (i32, i32) = (1, 3);
    let bytes = value.as_bytes();
    let mut index = 0;
    while bytes
        .get(index)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
    {
        index += 1;
    }
    let major_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == major_start || bytes.get(index) != Some(&b'.') {
        return Ok(FALLBACK);
    }
    let major_digits = &value[major_start..index];
    index += 1; // skip '.'
    let minor_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == minor_start {
        return Ok(FALLBACK);
    }
    let minor_digits = &value[minor_start..index];
    match (
        digit_run_to_i32(major_digits)?,
        digit_run_to_i32(minor_digits)?,
    ) {
        (Some(major), Some(minor)) => Ok((major, minor)),
        // major_digits/minor_digits are always non-empty ASCII-digit
        // substrings by construction (the preceding index ==
        // major_start/minor_start checks above already return FALLBACK for
        // an empty run), so digit_run_to_i32 can only return Some(_) or
        // propagate Err via `?`, never NoDigits's None.
        _ => Ok(FALLBACK), // cov:ignore: unreachable via this caller, see comment above
    }
}

fn digit_run_to_i32(digits: &str) -> Result<Option<i32>, Error> {
    match qpdf_string_to_int_checked(digits) {
        QpdfIntParse::Value(value) => Ok(Some(value)),
        QpdfIntParse::Overflow(message) => Err(Error::System(message)),
        // Unreachable via leading_major_minor, this function's only caller:
        // it always passes a non-empty ASCII-digit substring.
        QpdfIntParse::NoDigits => Ok(None), // cov:ignore: unreachable via this caller, see comment above
    }
}

/// The integer pair used by qpdf's `QPDFWriter::parseVersion` comparison.
/// Unlike [`PdfVersion`], this keeps the writer's raw version string separate
/// from its comparison values and accepts qpdf's lenient numeric conversion.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct QpdfVersionParts {
    pub(crate) major: i32,
    pub(crate) minor: i32,
}

impl QpdfVersionParts {
    pub(crate) const fn new(major: i32, minor: i32) -> Self {
        Self { major, minor }
    }
}

/// Mirrors qpdf's private `QPDFWriter::parseVersion`.
pub(crate) fn parse_qpdf_writer_version(value: &str) -> Option<QpdfVersionParts> {
    let major = qpdf_string_to_int(value)? as i32;
    let minor = match value.find('.') {
        Some(dot) => qpdf_string_to_int(&value[dot + 1..])? as i32,
        None => 0,
    };
    Some(QpdfVersionParts::new(major, minor))
}

fn qpdf_string_to_int(value: &str) -> Option<i64> {
    match qpdf_string_to_int_checked(value) {
        QpdfIntParse::NoDigits => Some(0),
        QpdfIntParse::Overflow(_) => None,
        QpdfIntParse::Value(value) => Some(i64::from(value)),
    }
}

/// Parses qpdf's job version option into the raw header version and an optional
/// extension level. qpdf splits at the second dot only; when that dot is absent
/// or has an empty tail, it preserves the complete input as the header value.
pub fn parse_pdf_version_spec(value: &str) -> Option<(String, i64)> {
    // qpdf copies the option into a NUL-terminated buffer before looking for
    // the first two dots (QPDFJob.cc:2833-2843). Keep the same C-string
    // boundary for callers that provide an embedded NUL.
    let value = value.split('\0').next().unwrap_or_default();
    let Some(first_dot) = value.find('.') else {
        // qpdf's `QPDFWriter::parseVersion` still calls `QUtil::string_to_int`
        // on the whole undotted string for the major component
        // (`QPDFWriter.cc:744-757`), so this value must be range-checked here
        // too, not only when a dot is present.
        parse_qpdf_writer_version(value)?;
        return Some((value.to_owned(), 0));
    };
    let second_dot = value[first_dot + 1..]
        .find('.')
        .map(|offset| first_dot + 1 + offset);

    let (version, extension_level) = match second_dot {
        Some(second_dot) if second_dot + 1 < value.len() => (
            &value[..second_dot],
            qpdf_string_to_int(&value[second_dot + 1..])?,
        ),
        _ => (value, 0),
    };
    let version = version.to_owned();
    parse_qpdf_writer_version(&version)?;
    Some((version, extension_level))
}

#[cfg(test)]
mod tests {
    use super::{leading_major_minor, PdfVersion};

    #[test]
    fn standard_version_strings_cover_writer_encryption_floors() {
        assert_eq!(PdfVersion::new(1, 3, 0).static_version_str(), Some("1.3"));
        assert_eq!(PdfVersion::new(1, 4, 0).static_version_str(), Some("1.4"));
        assert_eq!(PdfVersion::new(1, 5, 0).static_version_str(), Some("1.5"));
        assert_eq!(PdfVersion::new(1, 6, 0).static_version_str(), Some("1.6"));
        assert_eq!(PdfVersion::new(1, 7, 0).static_version_str(), Some("1.7"));
        assert_eq!(PdfVersion::new(1, 7, 8).static_version_str(), Some("1.7"));
        assert_eq!(PdfVersion::new(2, 0, 0).static_version_str(), None);
    }

    #[test]
    fn leading_major_minor_reads_the_ordinary_header_form() {
        assert_eq!(leading_major_minor("1.7").unwrap(), (1, 7));
        assert_eq!(leading_major_minor("2.0").unwrap(), (2, 0));
    }

    #[test]
    fn leading_major_minor_skips_leading_whitespace() {
        assert_eq!(leading_major_minor("  \t\n1.4").unwrap(), (1, 4));
    }

    #[test]
    fn leading_major_minor_ignores_trailing_bytes_after_the_prefix_match() {
        assert_eq!(leading_major_minor("1.7extra garbage").unwrap(), (1, 7));
    }

    #[test]
    fn leading_major_minor_falls_back_to_1_3_without_a_digit_run() {
        assert_eq!(leading_major_minor("").unwrap(), (1, 3));
        assert_eq!(leading_major_minor("abc").unwrap(), (1, 3));
    }

    #[test]
    fn leading_major_minor_falls_back_to_1_3_without_a_dot() {
        assert_eq!(leading_major_minor("17").unwrap(), (1, 3));
    }

    #[test]
    fn leading_major_minor_falls_back_to_1_3_with_an_empty_minor_run() {
        assert_eq!(leading_major_minor("1.").unwrap(), (1, 3));
        assert_eq!(leading_major_minor("1.abc").unwrap(), (1, 3));
    }

    /// A digit run within i32 range but outside `u8`'s prior range is no
    /// longer saturated: qpdf's own `int` fields store 300 as 300
    /// (`include/qpdf/PDFVersion.hh:60-62`).
    #[test]
    fn leading_major_minor_widens_a_digit_run_past_the_former_u8_range() {
        assert_eq!(leading_major_minor("300.7").unwrap(), (300, 7));
        assert_eq!(leading_major_minor("1.9999").unwrap(), (1, 9999));
    }

    /// A digit run outside i32 range propagates as qpdf's own uncaught
    /// `QUtil::string_to_int` failure, not a silent fallback
    /// (`QPDF.cc:2314,2317`).
    #[test]
    fn leading_major_minor_errors_on_i32_overflow() {
        let error = leading_major_minor("99999999999.7").unwrap_err();
        assert_eq!(
            error.to_string(),
            "integer out of range converting 99999999999 from a 8-byte signed type to a 4-byte signed type"
        );
    }
}
