//! qpdf correspondence: JSON.cc and JSONHandler.cc responsibilities split across the json module tree.
//! Public APIs: qpdf 11.9.0 `include/qpdf/JSON.hh` and
//! `libqpdf/qpdf/JSONHandler.hh`.
//!
//! qpdf Pipeline substitutions: `Pl_Base64` is the standard `base64` engine;
//! `Pl_Concatenate` and `Pl_String` are `Write` and `Vec<u8>`.

mod legacy;
mod value;
mod writer;

pub use legacy::{write, JsonValue};
pub use value::{Json, JsonError};
