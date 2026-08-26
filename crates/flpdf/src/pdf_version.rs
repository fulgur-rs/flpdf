//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.
//! Public API: qpdf 11.9.0 include/qpdf/PDFVersion.hh.

/// A PDF major/minor version paired with an optional extension level.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PdfVersion {
    major: u8,
    minor: u8,
    extension_level: i64,
}

pub(crate) const PDF_1_2: PdfVersion = PdfVersion::new(1, 2, 0);
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

/// Parses qpdf's job version syntax `M.m[.E]` into the header version and
/// optional extension level. The returned version is always the two-component
/// string that qpdf passes to `QPDFWriter`; the third component is never a PDF
/// header version.
pub fn parse_pdf_version_spec(value: &str) -> Option<(String, i64)> {
    let mut parts = value.split('.');
    let major = parts.next()?.parse::<u8>().ok()?;
    let minor = parts.next()?.parse::<u8>().ok()?;
    let extension_level = match parts.next() {
        None => 0,
        Some(level) if !level.is_empty() => level.parse::<i64>().ok()?,
        Some(_) => return None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((format!("{major}.{minor}"), extension_level))
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
