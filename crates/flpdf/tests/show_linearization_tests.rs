//! `show-linearization` decoder parity with qpdf `--show-linearization`.
//!
//! Two layers:
//!
//! * **Default feature (no qpdf needed):** decode qpdf's committed golden
//!   linearized PDFs and compare the dump against committed reference text
//!   (`tests/golden/references/<stem>/show-linearization.txt`), whose first
//!   line is normalized to the stable token `FIXTURE`. This exercises the
//!   decoder against qpdf's own output bytes without a live qpdf.
//! * **`qpdf-zlib-compat` feature (live qpdf):** run `qpdf
//!   --show-linearization` on the same committed golden and compare its full
//!   stdout, byte-for-byte, with `show_linearization_path` pointed at that same
//!   path. Using one path on both sides makes the filename line identical, so
//!   the whole output compares clean.
//!
//! The decoder reads qpdf's committed bytes either way, so the decoded field
//! values are identical regardless of which deflate backend flpdf links — the
//! `qpdf-zlib-compat` gate only matters for flpdf's *encoder*, not this reader.

use flpdf::linearization::show_linearization_bytes;
use std::path::{Path, PathBuf};

/// Path to a committed qpdf golden linearized PDF.
fn golden_pdf(stem: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("linearize.pdf")
}

/// Committed reference text whose first line is normalized to `FIXTURE`.
fn golden_text(stem: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references")
        .join(stem)
        .join("show-linearization.txt");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read text golden {path:?}: {e}"))
}

/// Decode `<stem>/linearize.pdf` and assert the dump (with a `FIXTURE`
/// display name) equals the committed reference text byte-for-byte.
fn assert_dump_matches_text_golden(stem: &str) {
    let bytes = std::fs::read(golden_pdf(stem))
        .unwrap_or_else(|e| panic!("read golden pdf for {stem}: {e}"));
    let dump = show_linearization_bytes(&bytes, "FIXTURE")
        .unwrap_or_else(|e| panic!("show_linearization_bytes({stem}): {e}"));
    let expected = golden_text(stem);
    assert_eq!(
        dump, expected,
        "{stem}: dump diverged from committed qpdf --show-linearization text golden"
    );
}

#[test]
fn one_page_dump_matches_text_golden() {
    assert_dump_matches_text_golden("one-page");
}

#[test]
fn two_page_dump_matches_text_golden() {
    assert_dump_matches_text_golden("two-page");
}

#[test]
fn three_page_dump_matches_text_golden() {
    assert_dump_matches_text_golden("three-page");
}

#[test]
fn non_linearized_reports_is_not_linearized() {
    // A non-linearized fixture: qpdf prints "<name> is not linearized" to
    // stdout and exits 0; show_linearization_bytes returns that line as Ok.
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
    let bytes = std::fs::read(&path).expect("read one-page fixture");
    let out = show_linearization_bytes(&bytes, "one-page.pdf").expect("not-linearized is Ok");
    assert_eq!(out, "one-page.pdf is not linearized\n");
}

// ---------------------------------------------------------------------------
// Live qpdf byte-for-byte parity (gated on qpdf-zlib-compat so the CI image
// that runs the gated suite is the one with qpdf 11.9.0 on PATH).
// ---------------------------------------------------------------------------

#[cfg(feature = "qpdf-zlib-compat")]
mod live_qpdf {
    use flpdf::linearization::show_linearization_path;
    use std::path::Path;
    use std::process::Command;

    fn golden_pdf(stem: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/golden/references")
            .join(stem)
            .join("linearize.pdf")
    }

    /// Run `qpdf --show-linearization <path>` and `show_linearization_path` on
    /// the SAME committed path; assert the full stdout is byte-identical.
    fn assert_parity_with_live_qpdf(stem: &str) {
        let path = golden_pdf(stem);
        let qpdf_out = Command::new("qpdf")
            .arg("--show-linearization")
            .arg(&path)
            .output()
            .expect("qpdf must be on PATH for the qpdf-zlib-compat suite");
        assert!(
            qpdf_out.status.success(),
            "qpdf --show-linearization {path:?} failed: {}",
            String::from_utf8_lossy(&qpdf_out.stderr)
        );
        let qpdf_stdout = String::from_utf8(qpdf_out.stdout).expect("qpdf output is UTF-8");

        let flpdf_out = show_linearization_path(&path)
            .unwrap_or_else(|e| panic!("show_linearization_path({stem}): {e}"));

        assert_eq!(
            flpdf_out, qpdf_stdout,
            "{stem}: flpdf show-linearization diverged from live qpdf --show-linearization"
        );
    }

    #[test]
    fn one_page_matches_live_qpdf() {
        assert_parity_with_live_qpdf("one-page");
    }

    #[test]
    fn two_page_matches_live_qpdf() {
        assert_parity_with_live_qpdf("two-page");
    }

    #[test]
    fn three_page_matches_live_qpdf() {
        assert_parity_with_live_qpdf("three-page");
    }

    /// A non-linearized input: flpdf must reproduce qpdf's stdout
    /// ("<path> is not linearized") byte-for-byte, on the same path.
    #[test]
    fn non_linearized_matches_live_qpdf() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/compat/one-page.pdf");
        let qpdf_out = Command::new("qpdf")
            .arg("--show-linearization")
            .arg(&path)
            .output()
            .expect("qpdf must be on PATH for the qpdf-zlib-compat suite");
        assert!(
            qpdf_out.status.success(),
            "qpdf must exit 0 on non-linearized input"
        );
        let qpdf_stdout = String::from_utf8(qpdf_out.stdout).expect("qpdf output is UTF-8");
        let flpdf_out = show_linearization_path(&path).expect("non-linearized is Ok, not an error");
        assert_eq!(flpdf_out, qpdf_stdout);
    }
}
