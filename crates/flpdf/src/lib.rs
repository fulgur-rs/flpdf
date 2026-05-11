//! `flpdf` is a pure-Rust PDF toolkit modeled on the qpdf workflow.
//!
//! The crate is organised as a few small layers that you can mix as needed:
//!
//! - [`Pdf`] is the parsed-but-lazy document handle. [`Pdf::open`] reads the trailer
//!   and cross-reference table, then resolves objects on demand via [`Pdf::resolve`].
//! - [`Object`], [`Dictionary`], [`Stream`], and [`ObjectRef`] are the data model.
//! - [`pages`], [`outline`], and [`fonts`] are traversal helpers built on top of
//!   `Pdf`. They mirror the read-only inspection surface that `qpdf --show-pages`,
//!   `--show-outline`, and `--show-fonts` provide.
//! - [`write_pdf`] performs an incremental rewrite (preserving the source bytes and
//!   appending an updated xref/trailer) and [`write_qdf`] produces a flat, qdf-style
//!   dump of every resolved object.
//! - [`check_reader`] reports diagnostics gathered during parsing/repair, returning a
//!   [`CheckReport`] of [`Diagnostic`]s.
//!
//! # End-to-end example
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, write_pdf, Pdf};
//!
//! let file = BufReader::new(File::open("input.pdf")?);
//! let mut pdf = Pdf::open(file)?;
//!
//! for object_ref in pages::page_refs(&mut pdf)? {
//!     println!("page: {object_ref}");
//! }
//!
//! let mut out = File::create("output.pdf")?;
//! write_pdf(&mut pdf, &mut out)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Errors flow through the unified [`Error`] enum and the crate-level [`Result`] alias,
//! except for the small [`object::ParseObjectRefError`] returned by
//! [`ObjectRef::parse`].

pub mod cache;
pub mod check;
pub mod diagnostics;
pub mod error;
pub mod filters;
pub mod fonts;
pub mod linearization;
pub mod object;
pub mod outline;
pub mod pages;
pub mod parser;
pub mod reader;
pub mod writer;
pub mod xref;

pub use cache::{CacheEntry, ObjectCache};
pub use check::{check_reader, check_reader_strict, CheckReport};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{Error, Result};
pub use object::{Dictionary, Object, ObjectRef, ParseObjectRefError, Stream};
pub use outline::OutlineItem;
pub use parser::parse_object;
pub use reader::Pdf;
pub use writer::{
    effective_pdf_version, parse_pdf_version, write_pdf, write_pdf_with_options, write_qdf,
    WriteOptions,
};
pub use xref::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort, load_xref_and_trailer_with_repair,
    LoadedXref, XrefForm, XrefOffset,
};

/// Crate version, mirroring `Cargo.toml`'s `[package].version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
