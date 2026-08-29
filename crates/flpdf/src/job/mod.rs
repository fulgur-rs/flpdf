//! qpdf correspondence: QPDFJob.cc command and JSON section orchestration.
//! Command-level operations corresponding to qpdf's `QPDFJob` layer.
//!
//! The module contains the shared qpdf 11.9.0 job lifecycle state and the JSON
//! output-selection responsibility from `QPDFJob::writeJSON`, the staged
//! `doJSON*` section builders, the page-selection merge boundary owned by
//! `QPDFJob::handlePageSpecs`, the attachment inspection consumers, the
//! `--overlay`/`--underlay` (`handleUnderOverlay`) and `--collate` orchestration,
//! the post-subset resource/reachability and AcroForm pruning
//! (`prune_after_subset`, `prune_acroform_after_subset`),
//! attachment listing/formatting, the `--rotate` spec and page-operation route,
//! and the `--check`
//! document-check consumer (`QPDFJob::doCheck`/`doInspection`).

mod acroform_field_prune;
mod attachment_list;
mod attachments;
mod check;
mod inspection;
mod json;
mod json_sections;
mod lifecycle;
mod outline_dest_remap;
mod overlay;
mod page_collate;
mod page_combine;
mod page_merge;
mod page_plan;
mod page_range;
mod page_specs;
mod page_split;
mod page_subset;
mod rotate;
mod rotate_spec;

pub use acroform_field_prune::{
    prune_acroform_after_subset, prune_acroform_after_subset_with_max_depth,
    DEFAULT_MAX_ACROFORM_DEPTH,
};
pub use attachment_list::{
    format_attachment_list, format_attachment_list_with_sink, list_attachment_info, AttachmentInfo,
};
pub use attachments::{
    add_attachment_from_path, ascii_filename_fallback, extract_attachment,
    extract_attachment_to_path, write_attachment, AttachmentAddOptions, AttachmentCopyOptions,
};
#[cfg(test)]
pub(crate) use check::check_bytes_for_test;
pub use check::CheckError;
pub use json::{write_json, JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData};
// Keep the internal aliases available to the json_inspect unit tests without
// exposing the staged implementation through the public job API.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use json::{
    write_qpdf_json_v2_selected_objects_to_output_with_options,
    write_qpdf_json_v2_selected_objects_with_options,
};
pub(crate) use json_sections::checksum_to_hex;
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use json_sections::{
    build_acroform_section, build_attachments_section, build_encrypt_section,
    build_outlines_section, build_pagelabels_section, build_pages_section,
};
#[cfg(test)]
pub(crate) use json_sections::{
    cf_method_string, collect_content_refs, collect_image_refs, parse_pdf_date,
};
pub use lifecycle::{JobExitCode, QPDFJob};
pub use outline_dest_remap::{remap_outline_and_dests, remap_outline_and_dests_with_max_depth};
pub use overlay::{
    apply_overlay_specs, overlay_verbose_report, OverlayKind, OverlaySpec, OverlayVerbosePage,
    OverlayVerboseSource,
};
pub use page_collate::collate;
pub use page_combine::{CombinedPage, CombinedPlan, InputSpec};
pub use page_merge::{merge_documents, MergeInput};
pub use page_plan::{PagePlan, SelectedPage};
pub use page_range::{Endpoint, PageRange, PageRangeEntry, Parity};
pub use page_specs::PageSpecInput;
pub use page_split::SplitPageOptions;
pub use rotate::{apply_rotate_to_pages, flatten_rotation_on_pages, RotateMode, RotateOp};
pub use rotate_spec::RotateSpec;
