#![no_main]

//! Fuzz object-stream parsing through the public, canonical reader path.
//!
//! `Pdf::get_all_objects` resolves every effective xref object, including
//! compressed objects whose type-2 entries cause the resolver to parse their
//! `/ObjStm` header pairs and member bodies. Parse errors are expected for
//! malformed PDFs; a panic, abort, sanitizer failure, out-of-memory condition,
//! or timeout is a defect.

use flpdf::PdfOpenOptions;
use libfuzzer_sys::fuzz_target;
use std::sync::Arc;

fuzz_target!(|input: &[u8]| {
    // `Pdf::open_mem` requires owned `'static` bytes. Keep the corpus input
    // shared through open and eager object resolution, without copying it per
    // compressed member.
    let bytes: Arc<[u8]> = Arc::from(input);
    let Ok(mut pdf) = flpdf::Pdf::open_mem_with_options(
        bytes,
        PdfOpenOptions {
            repair: false,
            suppress_warnings: true,
            ..PdfOpenOptions::default()
        },
    ) else {
        return;
    };

    // This is the qpdf-equivalent `QPDF::getAllObjects` boundary: it forces
    // the canonical resolver through every effective type-2 xref entry and
    // therefore exercises the ObjStm header and body parser without exporting
    // an internal parser helper from the library.
    let _ = pdf.get_all_objects();
});
