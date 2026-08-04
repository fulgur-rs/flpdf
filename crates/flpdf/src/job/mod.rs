//! qpdf correspondence: QPDFJob.cc command orchestration with only JSON output selection currently exposed.
//! Command-level operations corresponding to qpdf's `QPDFJob` layer.
//!
//! The current surface implements the JSON output-selection responsibility
//! from qpdf 11.9.0 `QPDFJob::writeJSON`.

mod json;

pub use json::{
    write_json, JsonJobError, JsonJobOptions, JsonJobOutput, JsonStreamData, UsageError,
};
