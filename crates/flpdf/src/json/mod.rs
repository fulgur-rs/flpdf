//! qpdf correspondence: JSON.cc and JSONHandler.cc responsibilities split across the json module tree.
//! Public APIs: qpdf 11.9.0 `include/qpdf/JSON.hh` and
//! `libqpdf/qpdf/JSONHandler.hh`.
//!
//! [`Reactor`] callbacks expose qpdf's incremental parse order and may consume
//! container items to keep them out of the returned tree.
//! Serialization APIs write to caller-supplied pipelines without finishing the
//! outer pipeline; callers retain ownership of that finish boundary.

mod handler;
mod message;
mod parser;
mod schema;
mod stdio;
mod value;
mod writer;

pub use handler::{JsonHandler, JsonHandlerError, WeakJsonHandler};
pub use message::JsonMessage;
pub use parser::{parse, parse_reader, Reactor};
pub use schema::SchemaFlags;
pub(crate) use stdio::QpdfStdioWriter;
pub use value::{Json, JsonError};
