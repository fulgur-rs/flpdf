#![no_main]

//! Whole-document fuzz harness: `open -> check -> write`.
//!
//! Mirrors qpdf's top-level `qpdf_fuzzer` (parse -> write). The core guarantee
//! under test is that arbitrary byte input never panics, aborts, or fails to
//! terminate; libFuzzer surfaces a violation as a crash (panic/abort/OOM) or a
//! hang (with `-timeout`). Returned `Err` values are the expected, correct
//! outcome for malformed input and are intentionally ignored.

use libfuzzer_sys::fuzz_target;
use flpdf::job::QPDFJob;
use flpdf::{PdfOpenOptions, QPDFLogger};
use std::io::Cursor;
use std::sync::{Arc, OnceLock};

/// A logger whose info/warn/error sinks all discard, shared across fuzz
/// iterations. `job::QPDFJob::check`'s banner and diagnostic output would
/// otherwise go to the job's default logger, which routes to real process
/// stdout/stderr (qpdf's own fuzzer never drives `QPDFJob` for the same
/// reason -- it calls `QPDF`/`QPDFWriter` directly with `Pl_Discard`).
fn discard_logger() -> QPDFLogger {
    static LOGGER: OnceLock<QPDFLogger> = OnceLock::new();
    LOGGER
        .get_or_init(|| {
            let logger = QPDFLogger::create();
            logger.set_info(Some(logger.discard()));
            logger.set_warn(Some(logger.discard()));
            logger.set_error(Some(logger.discard()));
            logger
        })
        .clone()
}

fuzz_target!(|data: &[u8]| {
    // `Pdf<R>` requires `R: 'static`, and libFuzzer lends `data` only for the
    // duration of this closure, so the input has to be owned. Share one
    // allocation across all three opens below rather than copying per open:
    // that is what `Arc<[u8]>` is for, and it keeps one copy per iteration in
    // the hot loop instead of three.
    let shared: Arc<[u8]> = Arc::from(data);

    // Repair-enabled open + validation path. QPDFJob opens with recovery and
    // runs the canonical qpdf document-check consumer, exercising the
    // recovery branches the strict open skips.
    let mut job = QPDFJob::new();
    job.set_logger(discard_logger());
    if let Ok(mut pdf) = job.open(
        Cursor::new(Arc::clone(&shared)),
        "fuzz-regression.pdf",
        PdfOpenOptions {
            repair: true,
            ..PdfOpenOptions::default()
        },
    ) {
        let _ = job.check(&mut pdf);
    }

    // Strict open + qpdf-shaped writer round-trip. Writing mutates the handle's
    // object/xref state, so it gets a freshly parsed handle rather than reusing
    // the handle from the repair-enabled check path.
    if let Ok(mut pdf) = flpdf::Pdf::open_mem(Arc::clone(&shared)) {
        let mut writer = flpdf::PdfWriter::new(&mut pdf);
        if writer.set_output_memory().is_ok() && writer.write().is_ok() {
            let _ = writer.get_buffer();
        }
    }
});
