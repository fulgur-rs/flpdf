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
//! The two opens below reach the strict and repair xref entry points through
//! the canonical reader (`Pdf::open` and `Pdf::open_with_options` with
//! `repair: true`). The owner-less `load_xref_and_trailer` family this target
//! used to call was removed with the second document owner it constructed;
//! qpdf likewise has no standalone xref loader, since one `QPDF` owns the xref
//! table and object cache (`include/qpdf/QPDF.hh:1465,1467`). Each call gets a
//! fresh cursor so a malformed input exercises both the ordinary table/stream
//! path and the line-scan recovery path. Returned errors are expected for
//! malformed input; a panic, abort, sanitizer failure, or timeout is the fuzz
//! failure.

use flpdf::PdfOpenOptions;
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

fuzz_target!(|data: &[u8]| {
    // `Pdf::open` requires a `'static` source, so the input is shared as owned
    // bytes -- the same shape `fuzz/fuzz_targets/roundtrip.rs` uses.
    let shared: Arc<[u8]> = Arc::from(data);

    // `PdfOpenOptions::default()` sets `repair: true`, so the strict pass has
    // to disable it explicitly; otherwise both passes would take the
    // recovery-enabled route and inputs that only fail with recovery
    // suppressed would stop being fuzzed.
    let _ = flpdf::Pdf::open_mem_with_options(
        Arc::clone(&shared),
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    );

    // `suppress_warnings` keeps the recovery warnings off the default logger's
    // stderr: most arbitrary inputs raise missing-header/missing-xref/
    // reconstruction warnings, and the removed standalone loader only
    // accumulated diagnostics rather than writing them out.
    let _ = flpdf::Pdf::open_mem_with_options(
        Arc::clone(&shared),
        PdfOpenOptions {
            repair: true,
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    );
});
