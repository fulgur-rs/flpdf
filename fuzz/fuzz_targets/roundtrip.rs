#![no_main]

//! Whole-document fuzz harness: `open -> check -> write`.
//!
//! Mirrors qpdf's top-level `qpdf_fuzzer` (parse -> write). The core guarantee
//! under test is that arbitrary byte input never panics, aborts, or fails to
//! terminate; libFuzzer surfaces a violation as a crash (panic/abort/OOM) or a
//! hang (with `-timeout`). Returned `Err` values are the expected, correct
//! outcome for malformed input and are intentionally ignored.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    // Repair-enabled open + validation path. `check_reader` opens internally,
    // runs the recovery heuristics, and reports diagnostics rather than
    // panicking, so it exercises the recovery branches the strict open skips.
    let _ = flpdf::check_reader(Cursor::new(data));

    // Strict open + writer round-trip. The incremental-append writer and the
    // full-rewrite writer each get a freshly parsed handle: writing mutates the
    // handle's object/xref state, so a shared handle would feed the second
    // writer a post-write document — a sequence no real consumer produces (each
    // writes once per open).
    if let Ok(mut pdf) = flpdf::Pdf::open_mem(data) {
        let mut incremental = Vec::new();
        let _ = flpdf::write_pdf(&mut pdf, &mut incremental);
    }
    if let Ok(mut pdf) = flpdf::Pdf::open_mem(data) {
        let mut rewritten = Vec::new();
        // `WriteOptions` is `#[non_exhaustive]`: build via `default()` then set
        // fields (struct-literal syntax is rejected outside the defining crate).
        let mut options = flpdf::WriteOptions::default();
        options.full_rewrite = true;
        let _ = flpdf::write_pdf_with_options(&mut pdf, &mut rewritten, &options);
    }
});
