//! Differential coverage for qpdf's fatal linearization-plan invariants.

use assert_cmd::Command;
use std::io::Write;
use std::process::Command as ProcessCommand;

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf version 11.9.0 is required for this differential test");
    }
    eprintln!("skipping: qpdf version 11.9.0 is not available");
    true
}

/// Reference the first page from a non-Root trailer key. qpdf's object-user
/// walk then gives that page both a page user and a trailer-key user, so the
/// first page is not lc_first_page_private and QPDF_linearization.cc:1194
/// stops the write.
fn first_page_shared_by_trailer_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let catalog = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
    let page = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{catalog:010} 00000 n \n{pages:010} 00000 n \n{page:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R /Extra 3 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

fn assert_linearize_rejected_like_qpdf(pdf_bytes: &[u8], tag: &str) {
    if skip_if_qpdf_missing() {
        return;
    }

    let tempdir = tempfile::tempdir().unwrap();
    let input = tempdir.path().join(format!("{tag}.pdf"));
    let qpdf_output = tempdir.path().join("qpdf-output.pdf");
    let flpdf_output = tempdir.path().join("flpdf-output.pdf");
    let mut file = std::fs::File::create(&input).unwrap();
    file.write_all(pdf_bytes).unwrap();

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .unwrap();
    let flpdf = Command::cargo_bin("flpdf")
        .unwrap()
        .env("FLPDF_PROGNAME", "qpdf")
        .args(["--deterministic-id", "--linearize"])
        .arg(&input)
        .arg(&flpdf_output)
        .output()
        .unwrap();

    assert_eq!(
        qpdf.status.code(),
        Some(2),
        "qpdf stderr: {:?}",
        qpdf.stderr
    );
    assert_eq!(flpdf.status.code(), qpdf.status.code());
    assert_eq!(flpdf.stderr, qpdf.stderr);
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        std::fs::metadata(&flpdf_output).unwrap().len(),
        0,
        "a failed linearization must not leave output bytes"
    );
    assert_eq!(
        std::fs::metadata(&qpdf_output).unwrap().len(),
        0,
        "qpdf's failed linearization output is empty"
    );
}

#[test]
fn linearize_rejects_a_first_page_that_is_not_private_like_qpdf() {
    assert_linearize_rejected_like_qpdf(&first_page_shared_by_trailer_pdf(), "first-page-shared");
}

/// Reference a later (non-first) page from a non-Root trailer key. qpdf's
/// object-user walk then gives that page both a page user and a trailer-key
/// user, so it is not lc_other_page_private and
/// QPDF_linearization.cc:1226-1231 stops the write.
fn other_page_shared_by_trailer_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let catalog = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n");
    let page1 = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );
    let page2 = pdf.len();
    pdf.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>\nendobj\n",
    );
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 5\n0000000000 65535 f \n{catalog:010} 00000 n \n{pages:010} 00000 n \n{page1:010} 00000 n \n{page2:010} 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R /Extra 4 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn linearize_rejects_an_other_page_that_is_not_private_like_qpdf() {
    assert_linearize_rejected_like_qpdf(&other_page_shared_by_trailer_pdf(), "other-page-shared");
}

/// Embed /Pages as a direct dictionary value instead of an indirect
/// reference. qpdf's object-user walk records `ou_root_key("/Pages")` only
/// for the indirect object a root key resolves to, so a direct /Pages value
/// leaves that user set empty and QPDF_linearization.cc:1279-1283 stops the
/// write.
fn direct_pages_tree_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let catalog = pdf.len();
    pdf.extend_from_slice(
        b"1 0 obj\n<< /Type /Catalog /Pages << /Type /Pages /Kids [3 0 R] /Count 1 >> >>\nendobj\n",
    );
    let page = pdf.len();
    pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /MediaBox [0 0 10 10] >>\nendobj\n");
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{catalog:010} 00000 n \n0000000000 65535 f \n{page:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn linearize_rejects_a_direct_pages_tree_like_qpdf() {
    assert_linearize_rejected_like_qpdf(&direct_pages_tree_pdf(), "direct-pages-tree");
}

/// An empty `/Pages` tree (`/Kids []`, `/Count 0`). qpdf's page-refs walk
/// then produces zero pages and `QPDF_linearization.cc:1188-1190`'s
/// `pages.empty()` check calls `stopOnError`, which throws a
/// `qpdf_e_damaged_pdf` `QPDFExc` carrying the input filename and the
/// source's last-read offset and no object description
/// (`libqpdf/QPDF.cc:2590-2592,2625-2628`). flpdf-a9m5 tracked a divergence
/// where this path was instead wrapped in `Error::Unsupported`'s
/// "unsupported PDF feature: " prefix, dropping that filename+offset
/// framing.
fn empty_pages_tree_pdf() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let catalog = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let pages = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!(
            "xref\n0 3\n0000000000 65535 f \n{catalog:010} 00000 n \n{pages:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    pdf
}

#[test]
fn linearize_rejects_an_empty_pages_tree_like_qpdf() {
    assert_linearize_rejected_like_qpdf(&empty_pages_tree_pdf(), "empty-pages-tree");
}

/// flpdf-ytqlr swept 171 `tests/fixtures/compat/*.pdf` fixtures through
/// `--linearize` against qpdf 11.9.0 and found 10 whose zero-visible-page
/// `stopOnError` framing (filename + `(offset N)`) was dropped in favor of
/// `Error::Unsupported`'s "unsupported PDF feature: " prefix. That divergence
/// no longer reproduces on `main` (fixed by 4afbd4a78's `qpdf_stop_on_error`
/// port, verified independently of this test), but nothing previously pinned
/// the fix for this specific fixture set, so a regression to the
/// `Error::Unsupported` wrapping would go uncaught. This locks in full
/// stderr byte-equality against real qpdf for all 10.
const NO_PAGES_FOUND_FIXTURES: &[&str] = &[
    "adbe-orphan-url",
    "compressible-stale-generation-alias",
    "direct-root-adbe",
    "make-indirect-historical-trailer",
    "nonmonotonic-objstm-index",
    "ocproperties-multiconfig",
    "one-page-ext-indirect",
    "one-page-no-ext",
    "one-page-xyzw-only",
    "preserve-no-source-objstm-xref",
];

#[test]
fn linearize_matches_qpdf_no_pages_found_framing_across_compat_fixtures() {
    for &name in NO_PAGES_FOUND_FIXTURES {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(format!("{name}.pdf"));
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
        assert_linearize_rejected_like_qpdf(&bytes, name);
    }
}
