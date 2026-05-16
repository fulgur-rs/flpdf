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

pub mod acroform_field_prune;
pub(crate) mod ascii85;
pub(crate) mod ascii_hex;
pub mod cache;
pub mod check;
pub mod page_document_helper;
pub mod content_stream;
pub mod diagnostics;
pub mod error;
pub mod filters;
pub mod fonts;
pub mod json;
pub mod json_inspect;
pub mod linearization;
pub mod object;
pub mod outline;
pub mod outline_dest_remap;
pub mod page_collate;
pub mod page_combine;
pub mod page_plan;
pub mod page_range;
pub mod page_rotate;
pub mod page_split;
pub mod page_tree_rebuild;
pub mod pages;
pub mod parser;
pub mod reader;
pub mod resources;
pub mod rotate_spec;
pub(crate) mod run_length;
pub mod subset_prune;
pub mod writer;
pub mod xref;

// Internal security primitives — not part of the public API.
pub(crate) mod security;

pub use acroform_field_prune::{
    prune_acroform_after_subset, prune_acroform_after_subset_with_max_depth,
    DEFAULT_MAX_ACROFORM_DEPTH,
};
pub use cache::{CacheEntry, ObjectCache};
pub use check::{check_reader, check_reader_strict, check_reader_with_options, CheckReport};
pub use content_stream::{
    normalize_content_stream, ContentParseOptions, ContentStreamParser, ContentToken,
};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use error::{EncryptedError, Error, Result};
pub use object::{Dictionary, Object, ObjectRef, ParseObjectRefError, Stream};
pub use outline::OutlineItem;
pub use outline_dest_remap::{remap_outline_and_dests, remap_outline_and_dests_with_max_depth};
pub use page_collate::collate;
pub use page_combine::{CombinedPage, CombinedPlan, InputSpec};
pub use page_plan::{PagePlan, SelectedPage};
pub use page_range::{Endpoint, PageRange, PageRangeEntry, Parity};
pub use page_rotate::{
    apply_rotate_to_pages, compose_rotate, normalize_rotate, resolve_inherited_rotate,
    resolve_inherited_rotate_with_max_depth, RotateMode, RotateOp,
};
pub use page_document_helper::PageDocumentHelper;
pub use page_split::{digit_width, split_output_path, split_pages};
pub use page_tree_rebuild::{rebuild_page_tree, rebuild_page_tree_with_max_depth, RebuildResult};
pub use parser::parse_object;
pub use reader::{EncryptionInfo, Pdf, PdfOpenOptions, Permissions};
pub use resources::{remove_unreferenced_resources, RemoveUnreferencedResources};
pub use rotate_spec::RotateSpec;
pub use security::password::PasswordMode;
pub use subset_prune::prune_after_subset;
pub use writer::{
    apply_stream_compress_policy, effective_pdf_version, parse_pdf_version, write_pdf,
    write_pdf_with_options, write_qdf, write_stream_to_buf, CompressStreams,
    NewlineBeforeEndstream, ObjectStreamMode, WriteOptions,
};
pub use xref::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort, load_xref_and_trailer_with_repair,
    LoadedXref, XrefForm, XrefOffset,
};

/// Crate version, mirroring `Cargo.toml`'s `[package].version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
