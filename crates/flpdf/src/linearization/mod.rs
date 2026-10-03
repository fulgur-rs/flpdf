//! Public linearization inspection APIs and crate-private writer machinery.
//!
//! qpdf correspondence: QPDF.hh public linearization inspection methods.
//!
//! `QPDF::checkLinearization` and `QPDF::showLinearizationData` are public
//! inspection operations. Planning, hint-table construction, renumbering, and
//! back-patching are writer internals in qpdf and remain crate-private here.

pub(crate) mod back_patch;
pub mod check;
pub(crate) mod hint_page;
pub(crate) mod hint_shared;
pub(crate) mod hint_stream;
pub(crate) mod part1;
pub(crate) mod plan;
pub(crate) mod renumber;
pub mod show;
pub(crate) mod writer;

pub use check::{
    check_linearization, check_linearization_bytes, check_linearization_path, CheckResult,
    LinearizationCheckError,
};
pub(crate) use check::{
    check_linearization_parameters, check_linearization_warnings, LinearizationParameterCheck,
};
pub use show::{
    show_linearization_bytes, show_linearization_bytes_with_warnings, show_linearization_path,
    show_linearization_path_with_warnings, show_linearization_pdf_with_warnings,
    ShowLinearizationError, ShowLinearizationOutput,
};
#[cfg(test)]
mod linearize_objstm_generate_tests;

#[cfg(all(test, feature = "qpdf-zlib-compat"))]
mod plan_parity_tests;
