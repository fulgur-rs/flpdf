//! qpdf correspondence: QPDFJob.cc command orchestration with only JSON output selection currently exposed.
//! Command-level operations corresponding to qpdf's `QPDFJob` layer.
//!
//! The module contains the shared qpdf 11.9.0 job lifecycle state and the JSON
//! output-selection responsibility from `QPDFJob::writeJSON`.

mod json;
mod lifecycle;

pub use json::{
    write_json, JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData, UsageError,
};
pub use lifecycle::{JobExitCode, QPDFJob};
