//! qpdf correspondence: QPDFJob.cc command and JSON section orchestration.
//! Command-level operations corresponding to qpdf's `QPDFJob` layer.
//!
//! The module contains the shared qpdf 11.9.0 job lifecycle state and the JSON
//! output-selection responsibility from `QPDFJob::writeJSON`, together with
//! the staged `doJSON*` section builders.

mod json;
mod json_sections;
mod lifecycle;

pub use json::{
    write_json, write_qpdf_json_v2_selected_objects_to_output_with_options,
    write_qpdf_json_v2_selected_objects_with_options, JsonJobError, JsonJobOptions, JsonJobOutput,
    JsonStreamData, UsageError,
};
pub(crate) use json_sections::checksum_to_hex;
pub use json_sections::{
    build_acroform_section, build_attachments_section, build_encrypt_section,
    build_outlines_section, build_pagelabels_section, build_pages_section,
};
#[cfg(test)]
pub(crate) use json_sections::{
    cf_method_string, collect_content_refs, collect_image_refs, parse_pdf_date,
};
pub use lifecycle::{JobExitCode, QPDFJob};
