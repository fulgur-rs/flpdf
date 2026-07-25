//! Mirrors qpdf 11.9.0 libqpdf/PDFVersion.cc.
//! Public API: qpdf 11.9.0 include/qpdf/PDFVersion.hh.

/// A PDF major/minor version paired with an optional extension level.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PdfVersion {
    major: u8,
    minor: u8,
    extension_level: i64,
}

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
}
