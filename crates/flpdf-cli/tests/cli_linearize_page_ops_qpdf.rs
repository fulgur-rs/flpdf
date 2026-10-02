//! qpdf 11.9.0 parity for linearized page-operation output.
//!
//! qpdf applies page selection/rotation/transformation before its canonical
//! writer settings, and applies linearization to every split chunk. These
//! cases guard that ordering at the CLI boundary.

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process::{Command as ShellCommand, Output};

const COMPAT: &str = "../../tests/fixtures/compat";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(COMPAT)
        .join(name)
}

fn skip_if_qpdf_missing() -> bool {
    if ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf is required for linearize page-operation parity tests on CI");
    }
    eprintln!("skipping: qpdf is not available");
    true
}

fn run_qpdf(args: &[&str]) -> Output {
    ShellCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_linearized(path: &Path, label: &str) {
    let output = run_qpdf(&["--check-linearization", path.to_str().unwrap()]);
    assert_success(&output, label);
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        report.contains("no linearization errors"),
        "{label} is not cleanly linearized: {report}"
    );
}

fn single_page_pdf_with_content_filter(filter_name: &str, payload: &[u8]) -> Vec<u8> {
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let off1 = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let off2 = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let off3 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>\nendobj\n",
    );
    let off4 = pdf.len();
    pdf.extend_from_slice(
        format!(
            "4 0 obj\n<< /Filter /{filter_name} /Length {} >>\nstream\n",
            payload.len(),
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(payload);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_start = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n{off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

#[test]
fn top_level_pages_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("three-page.pdf");
    let qpdf_output = temp.path().join("qpdf-pages.pdf");
    let flpdf_output = temp.path().join("flpdf-pages.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        input,
        "--pages",
        input,
        "1-2",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --pages");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        input,
        "--pages",
        input,
        "1-2",
        "--",
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --linearize --pages");
    assert_linearized(&qpdf_output, "qpdf --pages output");
    assert_linearized(&flpdf_output, "flpdf --pages output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized --pages output must match qpdf"
    );
}

#[test]
fn top_level_pages_linearize_preserves_unknown_content_filter_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("unknown-content-filter.pdf");
    std::fs::write(
        &input,
        single_page_pdf_with_content_filter(
            "PlateDecode",
            b"012345678901234567890123456789012345678901234567",
        ),
    )
    .unwrap();
    let qpdf_output = temp.path().join("qpdf-pages.pdf");
    let flpdf_output = temp.path().join("flpdf-pages.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(
        &qpdf,
        "qpdf --linearize --pages with unknown content filter",
    );

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1",
        "--",
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(
        &flpdf,
        "flpdf --linearize --pages with unknown content filter",
    );
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized page extraction must preserve unfilterable content exactly like qpdf"
    );
}

#[test]
fn top_level_pages_linearize_retries_invalid_flate_content_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("invalid-flate-content.pdf");
    std::fs::write(
        &input,
        single_page_pdf_with_content_filter("FlateDecode", b"this is not valid zlib data at all"),
    )
    .unwrap();
    let qpdf_output = temp.path().join("qpdf-pages.pdf");
    let flpdf_output = temp.path().join("flpdf-pages.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf should write with warnings: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1",
        "--",
        flpdf_output.to_str().unwrap(),
    ]);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr).replace("qpdf:", "flpdf:");
    assert_eq!(String::from_utf8_lossy(&flpdf.stderr), qpdf_stderr);
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized page extraction must retry invalid stream decoding exactly like qpdf"
    );
}

#[test]
fn top_level_pages_linearize_retries_truncated_flate_content_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("truncated-flate-content.pdf");
    std::fs::write(
        &input,
        single_page_pdf_with_content_filter("FlateDecode", &[0x78]),
    )
    .unwrap();
    let qpdf_output = temp.path().join("qpdf-pages.pdf");
    let flpdf_output = temp.path().join("flpdf-pages.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert_eq!(
        qpdf.status.code(),
        Some(3),
        "qpdf should write with warnings: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1",
        "--",
        flpdf_output.to_str().unwrap(),
    ]);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "flpdf stderr: {}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr).replace("qpdf:", "flpdf:");
    assert_eq!(String::from_utf8_lossy(&flpdf.stderr), qpdf_stderr);
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized page extraction must retry truncated stream decoding exactly like qpdf"
    );
}

#[test]
fn top_level_rotate_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("three-page.pdf");
    let qpdf_output = temp.path().join("qpdf-rotate.pdf");
    let flpdf_output = temp.path().join("flpdf-rotate.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--rotate=+90",
        input,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --rotate");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--rotate=+90",
        input,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --linearize --rotate");
    assert_linearized(&qpdf_output, "qpdf --rotate output");
    assert_linearized(&flpdf_output, "flpdf --rotate output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized --rotate output must match qpdf"
    );
}

#[test]
fn rewrite_flatten_rotation_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("one-page-r90.pdf");
    let qpdf_output = temp.path().join("qpdf-flatten.pdf");
    let flpdf_output = temp.path().join("flpdf-flatten.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--flatten-rotation",
        input,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --flatten-rotation");

    let flpdf = run_flpdf(&[
        "rewrite",
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--flatten-rotation",
        input,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf rewrite --linearize --flatten-rotation");
    assert_linearized(&qpdf_output, "qpdf --flatten-rotation output");
    assert_linearized(&flpdf_output, "flpdf --flatten-rotation output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized --flatten-rotation output must match qpdf"
    );
}

#[test]
fn rewrite_pages_linearize_normalize_content_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("three-page.pdf");
    let qpdf_output = temp.path().join("qpdf-rewrite-pages.pdf");
    let flpdf_output = temp.path().join("flpdf-rewrite-pages.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1-2",
        "--",
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --normalize-content --pages");

    let flpdf = run_flpdf(&[
        "rewrite",
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--normalize-content=y",
        input,
        "--pages",
        input,
        "1-2",
        "--",
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(
        &flpdf,
        "flpdf rewrite --linearize --normalize-content --pages",
    );
    assert_linearized(&qpdf_output, "qpdf rewrite --pages output");
    assert_linearized(&flpdf_output, "flpdf rewrite --pages output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "rewrite page extraction with linearized normalization must match qpdf"
    );
}

#[test]
fn top_level_flatten_rotation_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("one-page-r90.pdf");
    let qpdf_output = temp.path().join("qpdf-top-level-flatten.pdf");
    let flpdf_output = temp.path().join("flpdf-top-level-flatten.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--flatten-rotation",
        input,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --flatten-rotation top-level");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--flatten-rotation",
        input,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --linearize --flatten-rotation top-level");
    assert_linearized(&qpdf_output, "qpdf top-level flatten output");
    assert_linearized(&flpdf_output, "flpdf top-level flatten output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "top-level linearized --flatten-rotation output must match qpdf"
    );
}

#[test]
fn top_level_coalesce_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = fixture("multi-contents-one-page.pdf");
    let qpdf_output = temp.path().join("qpdf-coalesce.pdf");
    let flpdf_output = temp.path().join("flpdf-coalesce.pdf");
    let input = input.to_str().unwrap();

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--coalesce-contents",
        "--linearize",
        input,
        qpdf_output.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --coalesce-contents --linearize");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--coalesce-contents",
        "--linearize",
        input,
        flpdf_output.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --coalesce-contents --linearize");
    assert_linearized(&qpdf_output, "qpdf coalesced output");
    assert_linearized(&flpdf_output, "flpdf coalesced output");
    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "top-level coalesce+linearize output must match qpdf"
    );
}

#[test]
fn top_level_split_pages_linearizes_every_chunk_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let qpdf_dir = temp.path().join("qpdf");
    let flpdf_dir = temp.path().join("flpdf");
    std::fs::create_dir(&qpdf_dir).unwrap();
    std::fs::create_dir(&flpdf_dir).unwrap();
    let input = fixture("three-page.pdf");
    let input = input.to_str().unwrap();
    let qpdf_template = qpdf_dir.join("out.pdf");
    let flpdf_template = flpdf_dir.join("out.pdf");

    let qpdf = run_qpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--split-pages=1",
        input,
        qpdf_template.to_str().unwrap(),
    ]);
    assert_success(&qpdf, "qpdf --linearize --split-pages");

    let flpdf = run_flpdf(&[
        "--static-id",
        "--stream-data=uncompress",
        "--linearize",
        "--split-pages=1",
        input,
        flpdf_template.to_str().unwrap(),
    ]);
    assert_success(&flpdf, "flpdf --linearize --split-pages");

    for page in 1..=3 {
        let qpdf_chunk = qpdf_dir.join(format!("out-{page}.pdf"));
        let flpdf_chunk = flpdf_dir.join(format!("out-{page}.pdf"));
        assert!(qpdf_chunk.exists(), "qpdf chunk {page} must exist");
        assert!(flpdf_chunk.exists(), "flpdf chunk {page} must exist");
        assert_linearized(&qpdf_chunk, &format!("qpdf split chunk {page}"));
        assert_linearized(&flpdf_chunk, &format!("flpdf split chunk {page}"));
        assert_eq!(
            std::fs::read(&flpdf_chunk).unwrap(),
            std::fs::read(&qpdf_chunk).unwrap(),
            "linearized split chunk {page} must match qpdf"
        );
    }
}

/// `--qdf --linearize --split-pages`: qpdf clears QDF on every linearized
/// chunk writer (`QPDFWriter.cc:2068-2080`), so `--stream-data=preserve`
/// keeps the source stream filters. flpdf's internal memory rewrite that
/// feeds the split must therefore not run in QDF mode either.
#[test]
fn top_level_qdf_linearize_split_pages_preserve_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let qpdf_dir = temp.path().join("qpdf");
    let flpdf_dir = temp.path().join("flpdf");
    std::fs::create_dir(&qpdf_dir).unwrap();
    std::fs::create_dir(&flpdf_dir).unwrap();
    let input = fixture("three-page.pdf");
    let input = input.to_str().unwrap();
    let qpdf_template = qpdf_dir.join("out.pdf");
    let flpdf_template = flpdf_dir.join("out.pdf");
    let args = [
        "--static-id",
        "--qdf",
        "--linearize",
        "--stream-data=preserve",
        "--split-pages=1",
    ];

    let qpdf = run_qpdf(&[&args[..], &[input, qpdf_template.to_str().unwrap()]].concat());
    assert_success(&qpdf, "qpdf --qdf --linearize --split-pages");
    let flpdf = run_flpdf(&[&args[..], &[input, flpdf_template.to_str().unwrap()]].concat());
    assert_success(&flpdf, "flpdf --qdf --linearize --split-pages");

    for page in 1..=3 {
        let qpdf_chunk = qpdf_dir.join(format!("out-{page}.pdf"));
        let flpdf_chunk = flpdf_dir.join(format!("out-{page}.pdf"));
        let qpdf_bytes = std::fs::read(&qpdf_chunk).unwrap();
        assert!(
            qpdf_bytes
                .windows(b"/FlateDecode".len())
                .any(|part| part == b"/FlateDecode"),
            "qpdf chunk {page} keeps the preserved source filter"
        );
        assert_linearized(&flpdf_chunk, &format!("flpdf qdf linearized chunk {page}"));
        assert_eq!(
            std::fs::read(&flpdf_chunk).unwrap(),
            qpdf_bytes,
            "qdf+linearize split chunk {page} with --stream-data=preserve must match qpdf"
        );
    }
}
