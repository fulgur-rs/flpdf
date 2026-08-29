//! qpdf correspondence: crate root aggregating multiple qpdf library components and flpdf-only APIs.
//! `flpdf` is a pure-Rust PDF toolkit modeled on the qpdf workflow.
//!
//! The crate is organised as a few small layers that you can mix as needed:
//!
//! - [`Pdf`] is the parsed-but-lazy document handle. [`Pdf::open`] reads the trailer
//!   and cross-reference table, then resolves objects on demand via [`Pdf::resolve`].
//! - [`Object`], [`Dictionary`], [`Stream`], and [`ObjectRef`] are the data model.
//! - [`pages`] and [`outline_object_helper`] are traversal helpers built on top of `Pdf`. They
//!   mirror the read-only inspection surface that `qpdf --show-pages` and
//!   `--json-key=outlines` provide.
//! - [`PdfWriter`] configures the one fresh full-rewrite output with qpdf-shaped
//!   settings.
//! - [`job::QPDFJob::check`] owns the qpdf-compatible document-check lifecycle and
//!   warning/exit-status boundary.
//!
//! # End-to-end example
//!
//! ```no_run
//! use std::fs::File;
//! use std::io::BufReader;
//! use flpdf::{pages, Pdf, PdfWriter};
//!
//! let file = BufReader::new(File::open("input.pdf")?);
//! let mut pdf = Pdf::open(file)?;
//!
//! for object_ref in pages::page_refs(&mut pdf)? {
//!     println!("page: {object_ref}");
//! }
//!
//! let mut writer = PdfWriter::new(&mut pdf);
//! writer.set_output_file("output.pdf")?;
//! writer.write()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Errors flow through the unified [`Error`] enum and the crate-level [`Result`] alias,
//! except for the small [`object::ParseObjectRefError`] returned by
//! [`ObjectRef::parse`].
//!
//! # Known limitations
//!
//! - **Outline and page-label preservation.** Support scope varies by
//!   operation:
//!
//!   - [`PdfWriter`] output (with no page selection) preserves both destination
//!     sources — the legacy catalog `/Dests` dictionary and the modern
//!     `/Names /Dests` name tree (ISO 32000-2 §7.9.6, §12.3.2.3); deeply
//!     nested outlines (walks are iterative); all five `/A` action subtypes
//!     (`GoTo`, `GoToR`, `URI`, `Launch`, `Named`) plus their `/Next`
//!     chains (ISO 32000-2 §12.6.2); the `/SE` structure-element link
//!     (ISO 32000-2 §14.7); and the `/PageLabels` number-tree ranges
//!     (ISO 32000-2 §7.9.7).
//!   - [`rebuild_page_tree`] alone only rewrites the `/Pages` tree; it
//!     does NOT remap outline `/Dest` refs, does NOT drop `/Dests`
//!     entries whose targets went away, and does NOT reconstruct
//!     `/PageLabels`. Serializing only `rebuild_page_tree`'s output
//!     leaves stale destinations and label ranges pointing at pages no
//!     longer in the tree. Pair it with
//!     [`job::remap_outline_and_dests`] and (when labels matter)
//!     [`PageLabelDocumentHelper::labels_for_selection`] +
//!     [`PageLabelDocumentHelper::write_reconstructed_labels`] — the
//!     [`extract_pages`] and [`merge_documents`] pipelines assemble
//!     that combination for you.
//!   - [`merge_documents`] preserves outlines and both dest sources for
//!     the PRIMARY input only (matching qpdf `--pages`), and reconstructs
//!     `/PageLabels` across every input's selected pages.
//!   - [`extract_pages`] returns a *minimal* new document that intentionally
//!     omits catalog-level navigation — `/Outlines`, catalog `/Dests`, and
//!     `/Names /Dests` are all dropped. Only `/PageLabels` is reconstructed
//!     for the selection. Callers who need outlines/dests preserved should
//!     use [`merge_documents`] with a single input.
//! - **Raw outline keys are preserved.** Ordinary rewriting keeps `/SE`,
//!   `/Dest`, `/A`, and unknown outline dictionary keys as PDF objects rather
//!   than applying flpdf-specific validation or pruning policy.
//! - **`flpdf --json`'s `pagelabels` section** keeps qpdf's outer
//!   `{index, label}` entries, but represents each `label` as normalized
//!   `{first, prefix, style}` fields instead of qpdf JSON v2's raw page-label
//!   dictionary. The `outlines` section uses qpdf JSON v2's key layout.

// Mechanically enforce threat-model guarantee (a): no undefined behaviour.
// The explicit system-libjpeg compatibility backend lives in the optional
// flpdf-libjpeg-compat crate; this crate keeps unsafe code denied.
#![deny(unsafe_code)]

pub mod acroform_document_helper;
pub mod annotation_object_helper;
pub(crate) mod bit_stream;
pub(crate) mod bit_writer;
pub mod cache;
pub mod content_normalizer;
pub mod content_stream;
pub mod default_appearance;
pub mod diagnostics;
pub mod document_json;
pub mod embedded_files;
pub mod encryption;
pub mod engine;
pub mod error;
pub mod filespec_helper;
pub mod filters;
pub mod form_field_object_helper;
pub mod job;
pub mod json;
pub mod json_inspect;
pub mod linearization;
pub mod logger;
pub mod matrix;
mod nntree;
pub mod object;
pub mod object_copy;
mod object_handle;
pub mod objr_obj_annot_p;
pub(crate) mod optimization;
pub mod outline_document_helper;
pub mod outline_object_helper;
pub(crate) mod overlay_appearance_stream;
mod page_annotation_flatten;
pub mod page_document_helper;
pub mod page_extract;
pub(crate) mod page_form_xobject;
pub mod page_label_document_helper;
pub mod page_object_helper;
pub mod page_splice;
pub mod pages;
pub mod parser;
pub mod pdf;
pub mod pdf_string;
pub mod pdf_version;
pub mod pipeline;
pub mod qdf_fix;
mod qpdf_time;
pub mod qutil;
pub mod reader;
#[cfg(not(feature = "qtest-driver"))]
pub(crate) mod ref_chain;
#[cfg(feature = "qtest-driver")]
#[doc(hidden)]
pub mod ref_chain;
mod resource_finder;
mod resource_replacer;
pub mod resources;
pub mod signatures;
pub(crate) mod stream_filter;
pub mod struct_tree_pg;
pub mod thread_bead_p;
pub mod token_filter;
pub mod tokenizer;
pub mod writer;
pub mod xref;
pub mod xref_entry;

pub use logger::QPDFLogger;

pub use acroform_document_helper::{AcroFormDocumentHelper, AcroFormFieldInfo};
pub use annotation_object_helper::AnnotationObjectHelper;
pub use cache::{CacheEntry, ObjectCache};
pub use content_normalizer::{normalize_content_stream, ContentNormalization};
pub use content_stream::ObjectHandleParserCallbacks as ObjectParserCallbacks;
pub use content_stream::{
    parse_content_operations, parse_content_stream_data, ObjectHandleParserCallbacks, ParseControl,
    ParserCallbacks,
};
pub use default_appearance::{parse_default_appearance, DefaultAppearance, TextColor};
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use embedded_files::{
    delete_embedded_file, insert_embedded_file, list_embedded_files,
    list_embedded_files_with_max_depth, remove_attachment, EmbeddedFileDocumentHelper,
    DEFAULT_MAX_EMBEDDED_FILES_DEPTH,
};
pub use encryption::permissions::{Permissions, PermissionsConfig, PrintPermission};
pub use encryption::EncryptionInfo;
pub use encryption::{
    CopyEncryptionSource, EncryptMethod, EncryptParams, ObjectKeyAlg, PasswordMode,
};
pub use error::{EncryptedError, Error, Result, UsageError};
pub use filespec_helper::{
    encode_utf16be, format_pdf_date, md5_checksum, EmbeddedFileStream, FileParamDates, FileSpec,
    FileSpecBuilder,
};
pub use form_field_object_helper::FormFieldObjectHelper;
pub use job::{
    add_attachment_from_path, apply_overlay_specs, ascii_filename_fallback, collate,
    extract_attachment, extract_attachment_to_path, format_attachment_list,
    format_attachment_list_with_sink, list_attachment_info, merge_documents,
    overlay_verbose_report, prune_acroform_after_subset,
    prune_acroform_after_subset_with_max_depth, write_attachment, AttachmentInfo, CombinedPage,
    CombinedPlan, Endpoint, InputSpec, MergeInput, OverlayKind, OverlaySpec, OverlayVerbosePage,
    OverlayVerboseSource, PagePlan, PageRange, PageRangeEntry, Parity, RotateSpec, SelectedPage,
    DEFAULT_MAX_ACROFORM_DEPTH,
};
pub use job::{should_remove_unreferenced_resources, RemoveUnreferencedResources};
pub use matrix::{Matrix, Rectangle};
pub use nntree::{
    NameTree, NameTreeCursor, NumberTree, NumberTreeCursor, DEFAULT_MAX_TREE_DEPTH, LEAF_MAX,
};
pub use object::{Dictionary, Object, ObjectRef, ParseObjectRefError, Stream};
pub use object_handle::{
    ObjectHandle, StreamDataProvider, STREAM_ENCODE_COMPRESS, STREAM_ENCODE_NORMALIZE,
};
pub use objr_obj_annot_p::drop_objr_obj_annot_dangling_p;
pub use outline_document_helper::OutlineDocumentHelper;
pub use outline_object_helper::{OutlineId, OutlineItem, OutlineTree, OutlineTreeIter};
pub use page_document_helper::{PageDocumentHelper, PageInput};
pub use page_extract::{extract_page, extract_pages};
pub use page_label_document_helper::{
    merge_adjacent_ranges, merge_adjacent_ranges_with_prefix_presence, LabelRange, LabelStyle,
    PageLabelDocumentHelper,
};
pub use page_object_helper::{PageBox, PageObjectHelper};
pub use page_splice::{splice_pages, splice_pages_with_max_depth};
pub use pages::tree_rebuild::{rebuild_page_tree, rebuild_page_tree_with_max_depth, RebuildResult};
pub use parser::parse_object;
pub use pdf::Pdf;
pub use pdf_version::{parse_pdf_version, parse_pdf_version_spec, PdfVersion};
pub use pipeline::{Pipeline, PipelineError, PipelineHandle, PipelineResult};
pub use qdf_fix::fix_qdf;
pub use reader::PdfOpenOptions;
pub use signatures::{
    acroform_sig_flags, clear_sig_flags, disable_digital_signatures, remove_security_restrictions,
    signatures, signatures_with_max_depth, strip_signature_values, SignatureInfo,
    DEFAULT_MAX_SIGNATURE_FIELD_DEPTH, SIG_FLAGS_APPEND_ONLY, SIG_FLAGS_SIGNATURES_EXIST,
};
pub use struct_tree_pg::{
    drop_struct_elem_dangling_pg, drop_struct_elem_dangling_pg_with_max_depth,
    DEFAULT_MAX_STRUCT_TREE_DEPTH,
};
pub use thread_bead_p::drop_thread_bead_dangling_p;
pub use token_filter::{TokenFilter, TokenFilterOutput};
pub use tokenizer::{Token as ContentToken, TokenType as ContentTokenType};
#[cfg(any(test, feature = "qpdf-zlib-compat"))]
#[doc(hidden)]
pub use writer::V5Randomness;
pub use writer::{
    apply_stream_compress_policy, write_stream_to_buf, CompressStreams, DecodeLevel,
    NewlineBeforeEndstream, ObjectStreamMode, PdfWriter, StreamDataMode, WriterConfiguration,
};
pub use xref::{
    load_xref_and_trailer, load_xref_and_trailer_best_effort, load_xref_and_trailer_with_repair,
    LoadedXref, XrefForm,
};
pub use xref_entry::XrefEntry;

/// Crate version, mirroring `Cargo.toml`'s `[package].version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Return the pinned qpdf library version exposed by qpdf's
/// `QPDF::QPDFVersion` (`libqpdf/QPDF.cc:178-181`). This is deliberately
/// separate from [`version`], which reports flpdf's Cargo package version.
/// qpdf-shaped CLI and helper-process output must use this value.
pub fn qpdf_version() -> &'static str {
    "11.9.0"
}
