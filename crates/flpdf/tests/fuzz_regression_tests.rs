//! Regression gate for inputs discovered by the `fuzz/` cargo-fuzz harness.
//!
//! When the fuzzer finds an input that panics, aborts, or hangs, minimize it
//! and drop the bytes into `tests/fixtures/fuzz_regressions/`. This test replays
//! every file there through the same `open -> check -> write` pipeline as
//! `fuzz/fuzz_targets/roundtrip.rs`, so a fixed crash stays fixed. It runs on
//! stable (`cargo test -p flpdf`) with no nightly/libFuzzer dependency, making
//! it a durable gate independent of the fuzzer itself.

use std::io::Cursor;
use std::path::PathBuf;

/// Same pipeline as the `roundtrip` fuzz target. A panic here fails the test;
/// `Err` results are the expected outcome for malformed input and are ignored.
fn roundtrip(data: &[u8]) {
    let _ = flpdf::check_reader(Cursor::new(data));

    // Each writer gets a freshly parsed handle (writing mutates handle state, so
    // a shared handle would feed the second writer a post-write document — a
    // sequence no real consumer produces). Mirrors `fuzz/fuzz_targets/roundtrip.rs`.
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
