//! qpdf correspondence: JSON.cc and JSONHandler.cc responsibilities split across the json module tree.
//! Public APIs: qpdf 11.9.0 `include/qpdf/JSON.hh` and
//! `libqpdf/qpdf/JSONHandler.hh`.
//!
//! qpdf Pipeline substitutions: `Pl_Base64` is the standard `base64` engine;
//! `Pl_Concatenate` and `Pl_String` are `Write` and `Vec<u8>`.
//! [`Reactor`] callbacks expose qpdf's incremental parse order and may consume
//! container items to keep them out of the returned tree.

mod legacy;
mod parser;
mod value;
mod writer;

pub use legacy::{write, JsonValue};
pub use parser::{parse, parse_reader, Reactor};
pub use value::{Json, JsonError};
