use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

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
    pub fn parse(offset: usize, message: impl Into<String>) -> Self {
        Self::Parse {
            offset,
            message: message.into(),
        }
    }
}
