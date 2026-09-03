//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.
//! Public API: qpdf 11.9.0 include/qpdf/PDFVersion.hh.

use crate::qutil::{qpdf_string_to_int_checked, QpdfIntParse};

/// A PDF major/minor version paired with an optional extension level.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PdfVersion {
    major: u8,
    minor: u8,
    extension_level: i64,
}

pub(crate) const PDF_1_5: PdfVersion = PdfVersion::new(1, 5, 0);

impl PdfVersion {
    /// Creates a PDF version value.
    pub const fn new(major: u8, minor: u8, extension_level: i64) -> Self {
        Self {
            major,
            minor,
            extension_level,
        }
    }

    /// Parses the existing flpdf `M.m` version syntax with extension level 0.
    pub fn parse(value: &str) -> Option<Self> {
        let (major, minor) = value.split_once('.')?;
        Some(Self::new(major.parse().ok()?, minor.parse().ok()?, 0))
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
    pub const fn major(self) -> u8 {
        self.major
    }

    /// Returns the minor version.
    pub const fn minor(self) -> u8 {
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
    use super::PdfVersion;

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
}
