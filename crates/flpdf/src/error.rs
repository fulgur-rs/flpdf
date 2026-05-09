use thiserror::Error;

/// Crate-wide [`std::result::Result`] specialization.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by the public APIs of `flpdf`.
///
/// I/O failures bubble up via [`Error::Io`]. Structural problems (malformed tokens,
/// unexpected types, depth limits, oversized fields) use [`Error::Parse`] or
/// [`Error::Unsupported`]. [`Error::Missing`] is reserved for required dictionary
/// entries that the spec mandates, e.g. `/Root` on the trailer.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error at byte {offset}: {message}")]
    Parse { offset: usize, message: String },

    #[error("unsupported PDF feature: {0}")]
    Unsupported(String),

    #[error("missing required PDF entry: {0}")]
    Missing(&'static str),
}

impl Error {
    /// Convenience constructor for [`Error::Parse`].
    pub fn parse(offset: usize, message: impl Into<String>) -> Self {
        Self::Parse {
            offset,
            message: message.into(),
        }
    }
}
