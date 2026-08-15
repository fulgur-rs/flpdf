#![no_main]

//! Xref/trailer fuzz harness for strict parsing and qpdf-style recovery.
//!
//! qpdf 11.9.0 builds a whole-document `qpdf_fuzzer` alongside its focused
//! codec fuzzers (`fuzz/CMakeLists.txt:4-14`). Its safety contract is that
//! arbitrary input may produce the expected `QPDFExc` or `std::runtime_error`,
//! but not memory errors, segmentation faults, other exceptions, or abnormal
//! exits (`fuzz/qpdf_fuzzer.cc:184-209`). This target applies that contract to
//! flpdf's public xref boundary; it is a safety harness, not a qpdf output
//! compatibility test.
//!
//! The two loaders below correspond to the strict and repair entry points in
//! `crates/flpdf/src/xref.rs:657-708` (re-exported from
//! `crates/flpdf/src/lib.rs:289-293`). Each call gets a fresh cursor so a
//! malformed input exercises both the ordinary table/stream path and the
//! line-scan recovery path. Returned errors are expected for malformed input;
//! a panic, abort, sanitizer failure, or timeout is the fuzz failure.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let mut strict = Cursor::new(data);
    let _ = flpdf::load_xref_and_trailer(&mut strict);

    let mut repair = Cursor::new(data);
    let _ = flpdf::load_xref_and_trailer_with_repair(&mut repair, true);
});
