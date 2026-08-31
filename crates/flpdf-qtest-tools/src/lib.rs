//! Rust ports of the qpdf test-harness helper binaries that the
//! [flpdf-qtest](https://github.com/fulgur-rs/flpdf-qtest) acceptance suite
//! puts on `PATH`.
//!
//! qpdf keeps these helpers in separate source directories
//! (`compare-for-test/qpdf-test-compare.cc`, `qpdf/test_driver.cc`) but builds
//! them from one CMake project. This crate mirrors that: one build unit, one
//! binary per helper, with the argv-0 and stdout conventions they share
//! factored out rather than copied. The binary names are the interface the
//! harness depends on, so they stay fixed even when this package is renamed.

// Public modules the binaries re-use.
pub mod character_encoding;
pub mod clean;
pub mod common;
pub mod compare;
pub mod document_construction;
pub mod driver;
pub mod large_file;
pub mod metadata;
pub mod orchestrator;
pub mod output;
pub mod tokenizer_runner;

pub use orchestrator::compare_files;
