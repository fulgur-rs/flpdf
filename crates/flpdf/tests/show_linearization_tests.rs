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

use flpdf::linearization::{
    check_linearization_bytes, show_linearization_bytes, show_linearization_bytes_with_warnings,
};
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

/// Replace a numeric value in the hint-stream dictionary without changing the
/// serialized width or any later file offset.
fn replace_hint_dictionary_value(bytes: &mut [u8], key: &[u8], replacement: &[u8]) {
    let dictionary_marker = b"/Filter /FlateDecode /S ";
    let dictionary_start = bytes
        .windows(dictionary_marker.len())
        .position(|window| window == dictionary_marker)
        .expect("hint-stream dictionary");
    let key_start = dictionary_start
        + bytes[dictionary_start..]
            .windows(key.len())
            .position(|window| window == key)
            .expect("hint dictionary key")
        + key.len();
    let mut value_start = key_start;
    while bytes[value_start].is_ascii_whitespace() {
        value_start += 1;
    }
    let value_end = value_start
        + bytes[value_start..]
            .iter()
            .position(|byte| byte.is_ascii_whitespace())
            .expect("hint dictionary numeric value");
    assert!(replacement.len() <= value_end - value_start);
    let mut padded = vec![b' '; value_end - value_start];
    padded[..replacement.len()].copy_from_slice(replacement);
    bytes[value_start..value_end].copy_from_slice(&padded);
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

#[test]
fn soft_linearization_warnings_are_returned_without_dropping_the_dump() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat/linearized-one-page.pdf");
    let mut bytes = std::fs::read(fixture).expect("read linearized fixture");
    for (needle, replacement) in [
        (b"/O 6".as_slice(), b"/O 7".as_slice()),
        (b"/T 1523".as_slice(), b"/T 1522".as_slice()),
    ] {
        let start = bytes
            .windows(needle.len())
            .position(|window| window == needle)
            .expect("linearization parameter");
        bytes[start..start + needle.len()].copy_from_slice(replacement);
    }

    let result = show_linearization_bytes_with_warnings(&bytes, "mismatch.pdf")
        .expect("soft mismatches should still produce a dump");
    assert!(result
        .dump
        .windows(b"linearization data:".len())
        .any(|window| window == b"linearization data:"));
    assert_eq!(
        result.warnings,
        [
            b"first page object (/O) mismatch".to_vec(),
            b"space before first xref item (/T) mismatch (computed = 1524; file = 1522".to_vec(),
        ]
    );
}

#[test]
fn check_linearization_rejects_out_of_bounds_shared_hint_offset_directly() {
    let path = golden_pdf("linearized-one-page");
    let mut bytes = std::fs::read(path).expect("read linearized golden");
    replace_hint_dictionary_value(&mut bytes, b"/S ", b"99");

    let error = check_linearization_bytes(&bytes)
        .expect_err("the standalone checker must validate /S without show prechecking");
    let message = error.to_string();
    assert!(
        message.contains("hint stream /S offset (99) is out of bounds"),
        "unexpected /S boundary error: {message}"
    );
}

#[test]
fn check_linearization_rejects_out_of_bounds_outline_hint_offset_directly() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references/objstm-lin-outlines-80-80/linearize-classic.pdf");
    let mut bytes = std::fs::read(path).expect("read outlines golden");
    replace_hint_dictionary_value(&mut bytes, b"/O ", b"999");

    let error = check_linearization_bytes(&bytes)
        .expect_err("the standalone checker must validate /O without show prechecking");
    let message = error.to_string();
    assert!(
        message.contains("hint stream /O offset (999) is out of bounds"),
        "unexpected /O boundary error: {message}"
    );
}

/// A hint dictionary's `/O` (Outlines Hint Table offset) is decoded through
/// the same path as `/S` (Shared Objects). `objstm-lin-outlines-80-80`'s
/// classic golden carries a real Outlines hint table (qpdf
/// `--show-linearization` on this fixture prints an "Outlines Hint Table"
/// section with `first_object: 4`), unlike the smaller one/two/three-page
/// goldens used above, none of which have a catalog `/Outlines` entry.
#[test]
fn outlines_hint_table_decodes_from_a_fixture_that_has_one() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/references/objstm-lin-outlines-80-80/linearize-classic.pdf");
    let bytes = std::fs::read(&path).expect("read outlines golden");
    let dump = show_linearization_bytes(&bytes, "FIXTURE")
        .expect("a fixture with a real Outlines hint table must decode cleanly");
    assert!(
        dump.contains("Outlines Hint Table"),
        "decoding a hint dict with /O must produce the Outlines Hint Table section: {dump}"
    );
    assert!(
        dump.contains("first_object: 4"),
        "the decoded outline table's first_object must match qpdf --show-linearization: {dump}"
    );
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
