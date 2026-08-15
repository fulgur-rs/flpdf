//! Regression gate for inputs discovered by the `fuzz/` cargo-fuzz harness.
//!
//! When the fuzzer finds an input that panics, aborts, or hangs, minimize it
//! and drop the bytes into `tests/fixtures/fuzz_regressions/`. These tests
//! replay every file there through the same pipelines as
//! `fuzz/fuzz_targets/roundtrip.rs` and `fuzz/fuzz_targets/xref.rs`, so a fixed
//! crash stays fixed. They run on stable (`cargo test -p flpdf`) with no
//! nightly/libFuzzer dependency, making them durable gates independent of the
//! fuzzer itself.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

/// Same pipeline as the `roundtrip` fuzz target. A panic here fails the test;
/// `Err` results are the expected outcome for malformed input and are ignored.
fn roundtrip(data: &[u8]) {
    // One owned buffer shared by both opens, exactly as the fuzz target
    // does it: `Pdf<R>` requires `R: 'static`, so the input cannot be borrowed.
    let shared: Arc<[u8]> = Arc::from(data);

    let _ = flpdf::check_reader(Cursor::new(Arc::clone(&shared)));

    // The writer gets a freshly parsed handle (writing mutates handle state, so
    // a shared handle would feed it a post-write document — a sequence no real
    // consumer produces). Mirrors `fuzz/fuzz_targets/roundtrip.rs`.
    if let Ok(mut pdf) = flpdf::Pdf::open_mem(Arc::clone(&shared)) {
        let mut writer = flpdf::PdfWriter::new(&mut pdf);
        if writer.set_output_memory().is_ok() && writer.write().is_ok() {
            let _ = writer.get_buffer();
        }
    }
}

/// Same strict and repair pipeline as the `xref` fuzz target. A panic here
/// fails the test; `Err` results are the expected outcome for malformed input
/// and are ignored.
fn xref(data: &[u8]) {
    let mut strict = Cursor::new(data);
    let _ = flpdf::load_xref_and_trailer(&mut strict);

    let mut repair = Cursor::new(data);
    let _ = flpdf::load_xref_and_trailer_with_repair(&mut repair, true);
}

fn regressions_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fuzz_regressions")
}

#[test]
fn fuzz_regressions_do_not_panic() {
    let dir = regressions_dir();
    // The directory is committed (seeded with `minimal.pdf`), so it always
    // exists. Fail loudly if it is missing/renamed rather than returning early
    // and passing silently, which would defeat the `replayed > 0` check below.
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read fuzz regression dir {}: {e}", dir.display()));

    let mut replayed = 0usize;
    for entry in entries {
        let path = entry.expect("read fuzz regression dir entry").path();
        if !path.is_file() {
            continue;
        }
        let data = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read fuzz regression fixture {}: {e}", path.display()));
        roundtrip(&data);
        xref(&data);
        replayed += 1;
    }

    // The directory ships seeded with `minimal.pdf`, so a count of zero means the
    // fixtures went missing rather than "no regressions" — surface that.
    assert!(
        replayed > 0,
        "no fuzz regression fixtures found in {}",
        dir.display()
    );
}

#[test]
fn malformed_dictionary_does_not_reborrow_shared_state_while_writing() {
    // Captured from the roundtrip fuzzer. The malformed page dictionary and
    // duplicate stream object make writer-side null filtering resolve a child
    // while the containing ObjectValue is still borrowed. This must remain a
    // no-panic input even though the parser is expected to recover from it.
    let input = b"%PDF-1.7\n\
1 0 obj\n\
<< /Type /Catalog /Pages 2 0 R >>\n\
endobj\n\
2 0 obj\n\
<< /Type /Pages /Count 1 /Kids [3 0 R] >>\n\
endobj\n\
5 0 obj\n\
<< \x0fType /Page /Parent 2 0 R /MediaBox [0 0 1e3 .75] /Rotate +.5 /TrimBox [1. - .2 0 5? +.25 -1.5] /Contents 4 0 R >>\n\
endobj\n\
5 0 obj\n\
<< /Length\xe1 0 >>\n\
stream\n\n\
endstream\n\
endobj\n\
xref\n\
0 5\n\
0000000000 65535 f\n\
0000000009 00000 n\n\
0000000058 00000 n\n\
0000000115 00000 n\n\
0000000245 00000 n\n\
trailer\n\
<< /Size 5 /Root 1 0 R /I\x80fo 5 0 R >>\n\
startxref\n\
294\n\
%%EOF\n";

    roundtrip(input);
}
