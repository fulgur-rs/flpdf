//! `/PageLabels` reconstruction parity vs qpdf 11.9.0 for `--pages`.
//!
//! Truth source: `/usr/bin/qpdf` 11.9.0. When `--pages` selects a subset (or
//! reorders/duplicates) a document's pages, qpdf reconstructs `/PageLabels`
//! from scratch: one entry per selected page, in output order, built via
//! `QPDFPageLabelDocumentHelper::getLabelForPage` (`/St` always explicit) and
//! folded to drop entries redundant with the running sequence. This test runs
//! the SAME fixture and page selection through both `qpdf` and the `flpdf`
//! binary, then compares the resulting `/PageLabels` — read back through
//! flpdf's own reader (`Pdf::page_labels`) so the comparison does not depend
//! on exact object numbering or QDF-formatting differences between the two
//! tools' outputs.

use assert_cmd::Command;
use flpdf::Pdf;
use std::path::Path;
use std::process::Command as Shell;

/// `qpdf` binary path (the project's pinned truth source).
const QPDF: &str = "/usr/bin/qpdf";

/// The qpdf release this parity test's expected behaviour was derived from.
const EXPECTED_QPDF_VERSION: &str = "11.9.0";

fn qpdf_available() -> bool {
    if !Path::new(QPDF).exists() {
        return false;
    }
    match Shell::new(QPDF).arg("--version").output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.lines().next().map(str::trim)
                == Some(&format!("qpdf version {EXPECTED_QPDF_VERSION}"))
        }
        Err(_) => false,
    }
}

/// Four-page document with `/PageLabels`: roman lowercase from page 0,
/// decimal (restart at 1) from page 2.
fn labeled_four_page_pdf() -> Vec<u8> {
    let objects: &[(u32, &str)] = &[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /PageLabels \
             << /Nums [0 << /S /r >> 2 << /S /D /St 1 >>] >> >>",
        ),
        (
            2,
            "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R 6 0 R] /Count 4 >>",
        ),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (5, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ];
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = objects.iter().map(|(n, _)| *n).max().unwrap() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn prefixed_two_page_pdf() -> Vec<u8> {
    let objects: &[(u32, &str)] = &[
        (
            1,
            "<< /Type /Catalog /Pages 2 0 R /PageLabels << /Nums [0 << /S /D /P (Chapter-) /St 1 >>] >> >>",
        ),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ];
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = objects.iter().map(|(n, _)| *n).max().unwrap() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        out.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn read_ranges(path: &Path) -> Vec<(i64, flpdf::LabelRange)> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    let mut pdf = Pdf::open(std::io::BufReader::new(file))
        .unwrap_or_else(|e| panic!("flpdf could not parse {path:?}: {e}"));
    pdf.page_labels()
        .ranges()
        .unwrap_or_else(|e| panic!("read /PageLabels from {path:?}: {e}"))
}

fn qpdf_pagelabels_json(path: &Path) -> Vec<u8> {
    let output = Shell::new(QPDF)
        .args([
            "--json=2",
            "--json-key=pagelabels",
            path.to_str().unwrap(),
            "-",
        ])
        .output()
        .expect("qpdf should spawn");
    assert!(
        output.status.success(),
        "qpdf JSON output failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn flpdf_pagelabels_json(path: &Path) -> Vec<u8> {
    let output = Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--json=2", "--json-key=pagelabels", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "flpdf JSON output failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn cli_pages_preserves_explicit_empty_nonempty_and_absent_prefix() {
    if !qpdf_available() {
        eprintln!(
            "[SKIP cli_pages_pagelabels_qpdf] {} 11.9.0 not on PATH — set QPDF env or install to run",
            QPDF
        );
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let explicit_source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/json-diff/direct-outlines.pdf");
    let qpdf_explicit = tmp.path().join("qpdf_explicit.pdf");
    let status = Shell::new(QPDF)
        .args([
            explicit_source.to_str().unwrap(),
            "--pages",
            ".",
            "1-2",
            "--",
            qpdf_explicit.to_str().unwrap(),
        ])
        .status()
        .expect("qpdf should spawn");
    assert!(status.success(), "qpdf --pages should succeed");

    let flpdf_explicit = tmp.path().join("flpdf_explicit.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&explicit_source)
        .args(["--pages", ".", "1-2", "--"])
        .arg(&flpdf_explicit)
        .assert()
        .success();

    let qpdf_json = qpdf_pagelabels_json(&qpdf_explicit);
    let flpdf_json = flpdf_pagelabels_json(&flpdf_explicit);
    assert!(
        String::from_utf8_lossy(&qpdf_json).contains("\"/P\": \"u:\""),
        "the oracle fixture must exercise an explicitly empty /P: {}",
        String::from_utf8_lossy(&qpdf_json)
    );
    assert_eq!(
        flpdf_json, qpdf_json,
        "single-document --pages must preserve qpdf's raw /P presence"
    );

    // The same route must not manufacture /P for a source whose range never
    // had the key. This is the complementary absence case for the raw-key
    // distinction above.
    let absent_source = tmp.path().join("absent.pdf");
    std::fs::write(&absent_source, labeled_four_page_pdf()).unwrap();
    let qpdf_absent = tmp.path().join("qpdf_absent.pdf");
    let status = Shell::new(QPDF)
        .args([
            absent_source.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            "--",
            qpdf_absent.to_str().unwrap(),
        ])
        .status()
        .expect("qpdf should spawn");
    assert!(status.success(), "qpdf --pages should succeed");
    let flpdf_absent = tmp.path().join("flpdf_absent.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&absent_source)
        .args(["--pages", ".", "1", "--"])
        .arg(&flpdf_absent)
        .assert()
        .success();

    let qpdf_json = qpdf_pagelabels_json(&qpdf_absent);
    let flpdf_json = flpdf_pagelabels_json(&flpdf_absent);
    assert!(
        !String::from_utf8_lossy(&qpdf_json).contains("\"/P\""),
        "the absent-prefix fixture must not have /P: {}",
        String::from_utf8_lossy(&qpdf_json)
    );
    assert_eq!(
        flpdf_json, qpdf_json,
        "single-document --pages must keep an absent /P absent"
    );

    let prefixed_source = tmp.path().join("prefixed.pdf");
    std::fs::write(&prefixed_source, prefixed_two_page_pdf()).unwrap();
    let qpdf_prefixed = tmp.path().join("qpdf_prefixed.pdf");
    let status = Shell::new(QPDF)
        .args([
            prefixed_source.to_str().unwrap(),
            "--pages",
            ".",
            "1",
            "--",
            qpdf_prefixed.to_str().unwrap(),
        ])
        .status()
        .expect("qpdf should spawn");
    assert!(status.success(), "qpdf --pages should succeed");
    let flpdf_prefixed = tmp.path().join("flpdf_prefixed.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&prefixed_source)
        .args(["--pages", ".", "1", "--"])
        .arg(&flpdf_prefixed)
        .assert()
        .success();

    let qpdf_json = qpdf_pagelabels_json(&qpdf_prefixed);
    let flpdf_json = flpdf_pagelabels_json(&flpdf_prefixed);
    assert!(
        String::from_utf8_lossy(&qpdf_json).contains("\"/P\": \"u:Chapter-\""),
        "the non-empty-prefix fixture must retain /P: {}",
        String::from_utf8_lossy(&qpdf_json)
    );
    assert_eq!(
        flpdf_json, qpdf_json,
        "single-document --pages must preserve a non-empty /P"
    );
}

#[test]
fn cli_pages_subset_reconstructs_labels_like_qpdf() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_pages_pagelabels_qpdf] {} 11.9.0 not on PATH — set QPDF env or install to run", QPDF);
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("in.pdf");
    std::fs::write(&src, labeled_four_page_pdf()).unwrap();

    // Select 1-based pages 1 and 3 (0-based 0, 2): roman page + decimal page.
    let qpdf_out = tmp.path().join("qpdf_out.pdf");
    let status = Shell::new(QPDF)
        .args([
            src.to_str().unwrap(),
            "--pages",
            ".",
            "1,3",
            "--",
            qpdf_out.to_str().unwrap(),
        ])
        .status()
        .expect("qpdf should spawn");
    assert!(status.success(), "qpdf --pages should succeed");

    let flpdf_out = tmp.path().join("flpdf_out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&src)
        .args(["--pages", ".", "1,3", "--"])
        .arg(&flpdf_out)
        .assert()
        .success();

    let qpdf_ranges = read_ranges(&qpdf_out);
    let flpdf_ranges = read_ranges(&flpdf_out);
    assert_eq!(
        flpdf_ranges, qpdf_ranges,
        "flpdf's reconstructed /PageLabels must match qpdf's"
    );
    // Pin the expected shape explicitly too, so a change to either
    // side's fixture/behaviour surfaces as a clear diff.
    assert_eq!(
        flpdf_ranges,
        vec![
            (
                0,
                flpdf::LabelRange {
                    style: flpdf::LabelStyle::RomanLower,
                    prefix: String::new(),
                    start: 1
                }
            ),
            (
                1,
                flpdf::LabelRange {
                    style: flpdf::LabelStyle::Decimal,
                    prefix: String::new(),
                    start: 1
                }
            ),
        ]
    );
}

#[test]
fn cli_pages_without_source_labels_has_none() {
    if !qpdf_available() {
        eprintln!("[SKIP cli_pages_pagelabels_qpdf] {} 11.9.0 not on PATH — set QPDF env or install to run", QPDF);
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("in.pdf");
    // Same 4-page structure, but no /PageLabels in the catalog.
    let objects: &[(u32, &str)] = &[
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ];
    let mut bytes: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (n, body) in objects {
        offsets.insert(*n, bytes.len() as u64);
        bytes.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = bytes.len() as u64;
    let size = 5u32;
    bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for n in 1..size {
        bytes.extend_from_slice(format!("{:010} 00000 n \n", offsets[&n]).as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    std::fs::write(&src, bytes).unwrap();

    let flpdf_out = tmp.path().join("flpdf_out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg(&src)
        .args(["--pages", ".", "1", "--"])
        .arg(&flpdf_out)
        .assert()
        .success();

    let mut pdf = Pdf::open(std::io::BufReader::new(
        std::fs::File::open(&flpdf_out).unwrap(),
    ))
    .unwrap();
    // This test guards the CLI wrapper specifically: sister tests cover the
    // library API (page_extract_tests: extract_pages) and the split path
    // (page_merge_tests: merge with no labels). Kept per-layer so a bug
    // scoped to the CLI wiring cannot slip through by regressing only here.
    assert!(
        !pdf.page_labels().has_page_labels().unwrap(),
        "a source with no /PageLabels must not gain one"
    );
}
