//! qpdf correspondence: JSON.cc and JSONHandler.cc pipeline-native value, parse, callback, and serialization responsibilities.
//! Public APIs: qpdf 11.9.0 `include/qpdf/JSON.hh` and
//! `libqpdf/qpdf/JSONHandler.hh`.
//!
//! [`Reactor`] callbacks expose qpdf's incremental parse order and may consume
//! container items to keep them out of the returned tree.
//! Serialization APIs, including blob callbacks, write only to caller-supplied
//! pipelines without finishing the outer pipeline; callers retain ownership of
//! that finish boundary.

mod handler;
#[allow(dead_code)] // consumed by the JSONReactor slice in flpdf-3yn9.15.3
pub(crate) mod input;
#[cfg(test)]
mod input_tests;
mod message;
mod parser;
mod schema;
mod value;
mod writer;

pub use handler::{JsonHandler, JsonHandlerError, WeakJsonHandler};
pub use message::JsonMessage;
pub use parser::{parse, parse_reader, Reactor};
pub use schema::SchemaFlags;
pub use value::{Json, JsonError};
