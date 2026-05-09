pub mod cache;
pub mod check;
pub mod diagnostics;
pub mod error;
pub mod filters;
pub mod object;
pub mod parser;
pub mod reader;
pub mod writer;
pub mod xref;

pub use cache::{CacheEntry, ObjectCache};
pub use check::{check_reader, CheckReport};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, Stream};
pub use parser::parse_object;
pub use reader::Pdf;
pub use writer::{write_pdf, write_qdf};
pub use xref::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort, load_xref_and_trailer_with_repair,
    LoadedXref, XrefOffset,
};

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
