//! qpdf 11.9.0 byte parity for multi-source pages with generated ObjStms.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

const PRIMARY: &str = "../../tests/fixtures/compat/three-page.pdf";
const FOREIGN: &str = "../../tests/fixtures/compat/one-page.pdf";
const DUPLICATE_PRIMARY: &str = "../../tests/fixtures/compat/multi-contents-one-page.pdf";
const DUPLICATE_FOREIGN: &str = "../../tests/fixtures/compat/fxo-red.pdf";
const LINEARIZED_PRIMARY: &str = "../../tests/fixtures/compat/multi-contents-one-page.pdf";
const LINEARIZED_FOREIGN: &str = "../../tests/fixtures/compat/fxo-red.pdf";
const QDF_PRIMARY: &str = "../../tests/fixtures/compat/primary-objstm-exclusive-font.pdf";
const QDF_FOREIGN: &str = "../../tests/fixtures/compat/no-stream-one-page.pdf";
const OCCURRENCE_ORDER_PRIMARY: &str = "../../tests/fixtures/compat/three-page.pdf";
const OCCURRENCE_ORDER_FOREIGN: &str = "../../tests/fixtures/compat/two-page.pdf";
const ANNOTATION_ORDER_PRIMARY: &str = "../../tests/fixtures/compat/three-page.pdf";
const ANNOTATION_ORDER_FOREIGN: &str =
    "../../tests/fixtures/compat/form-fields-and-annotations.pdf";
const ANNOTATION_ORDER_LINK: &str = "../../tests/fixtures/compat/link-annot-no-acroform.pdf";
const ANNOTATION_ORDER_FXO: &str = "../../tests/fixtures/compat/fxo-red-with-existing-acroform.pdf";
const ANNOTATION_ORDER_DIRECT_DR: &str =
    "../../tests/fixtures/compat/form-fields-and-annotations-direct-dr.pdf";

/// Gate the differential probe on the pinned oracle, mirroring
/// `cli_linearize_multi_source_qpdf`: skip locally when qpdf 11.9.0 is not
/// installed, but keep it mandatory on CI. A different qpdf is not a parity
/// oracle, so it counts as missing.
fn skip_if_qpdf_missing() -> bool {
    let version = ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| {
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
        });
    if version
        .as_deref()
        .is_some_and(|stdout| stdout.lines().next() == Some("qpdf version 11.9.0"))
    {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for multi-source ObjStm order parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    true
}

fn run_qpdf(output: &Path) -> Output {
    ProcessCommand::new("qpdf")
        .args([
            "--static-id",
            "--object-streams=generate",
            PRIMARY,
            "--pages",
            PRIMARY,
            "1",
            FOREIGN,
            "--",
        ])
        .arg(output)
        .output()
        .expect("qpdf should spawn")
}

fn run_qpdf_duplicate_page(output: &Path) -> Output {
    ProcessCommand::new("qpdf")
        .args([
            "--static-id",
            "--newline-before-endstream=n",
            "--object-streams=generate",
            DUPLICATE_PRIMARY,
            "--pages",
            DUPLICATE_PRIMARY,
            "1,1",
            DUPLICATE_FOREIGN,
            "1",
            "--",
        ])
        .arg(output)
        .output()
        .expect("qpdf should spawn")
}

fn occurrence_order_page_args() -> [&'static str; 9] {
    [
        OCCURRENCE_ORDER_PRIMARY,
        "--pages",
        OCCURRENCE_ORDER_PRIMARY,
        "1",
        OCCURRENCE_ORDER_FOREIGN,
        "1",
        OCCURRENCE_ORDER_PRIMARY,
        "1",
        "--",
    ]
}

fn run_qpdf_duplicate_after_foreign(output: &Path, options: &[&str]) -> Output {
    let mut args = vec!["--static-id"];
    args.extend_from_slice(options);
    args.extend(occurrence_order_page_args());
    ProcessCommand::new("qpdf")
        .args(args)
        .arg(output)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf_duplicate_after_foreign(output: &Path, options: &[&str]) {
    let mut command = Command::cargo_bin("flpdf").unwrap();
    command
        .args(["--static-id"])
        .args(options)
        .args(occurrence_order_page_args())
        .arg(output)
        .assert()
        .success();
}

fn assert_duplicate_after_foreign_matches_qpdf(options: &[&str], message: &str) {
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf = run_qpdf_duplicate_after_foreign(&qpdf_output, options);
    assert!(
        qpdf.status.success(),
        "qpdf occurrence-order probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    run_flpdf_duplicate_after_foreign(&flpdf_output, options);

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "{message}"
    );
}

fn assert_annotated_linearized_matches_qpdf(page_args: &[&str], message: &str) {
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let mut qpdf_args = vec!["--static-id", "--linearize"];
    qpdf_args.extend_from_slice(page_args);
    let qpdf = ProcessCommand::new("qpdf")
        .args(qpdf_args)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf annotated linearization probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--static-id", "--linearize"])
        .args(page_args)
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "{message}"
    );
}

fn cross_section_shared_pdf() -> Vec<u8> {
    let mut objects = BTreeMap::new();
    objects.insert(1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.insert(
        2,
        b"<< /Type /Pages /Kids [4 0 R 5 0 R 6 0 R] /Count 3 >>".to_vec(),
    );
    objects.insert(
        4,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /FA 20 0 R >> >> /Contents 10 0 R >>".to_vec(),
    );
    objects.insert(
        5,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /FA 20 0 R /FB 7 0 R >> >> /Contents 11 0 R >>".to_vec(),
    );
    objects.insert(
        6,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /FB 7 0 R >> >> /Contents 12 0 R >>".to_vec(),
    );
    objects.insert(
        7,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".to_vec(),
    );
    objects.insert(
        20,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    );
    for (number, text) in [
        (10, b"BT /FA 12 Tf 20 100 Td (p1) Tj ET\n".as_slice()),
        (
            11,
            b"BT /FA 12 Tf 20 100 Td (p2a) Tj ET BT /FB 12 Tf 20 80 Td (p2b) Tj ET\n",
        ),
        (12, b"BT /FB 12 Tf 20 100 Td (p3) Tj ET\n"),
    ] {
        let mut stream = format!("<< /Length {} >>\nstream\n", text.len()).into_bytes();
        stream.extend_from_slice(text);
        stream.extend_from_slice(b"endstream");
        objects.insert(number, stream);
    }

    let max_object = *objects.keys().max().unwrap();
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![None; max_object as usize + 1];
    for (number, body) in objects {
        offsets[number as usize] = Some(bytes.len());
        writeln!(&mut bytes, "{number} 0 obj").unwrap();
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = bytes.len();
    writeln!(&mut bytes, "xref\n0 {}", max_object + 1).unwrap();
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) {
        match offset {
            Some(offset) => writeln!(&mut bytes, "{offset:010} 00000 n ").unwrap(),
            None => bytes.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    writeln!(
        &mut bytes,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF",
        max_object + 1
    )
    .unwrap();
    bytes
}

#[test]
fn classic_linearize_cross_section_shared_identifiers_match_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("cross-section-shared.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    std::fs::write(&input, cross_section_shared_pdf()).unwrap();

    let qpdf = ProcessCommand::new("qpdf")
        .args(["--static-id", "--linearize", "--object-streams=disable"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf cross-section probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--static-id", "--linearize", "--object-streams=disable"])
        .arg(&input)
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "classic cross-section shared identifiers must match qpdf"
    );
}

#[test]
fn annotated_multi_source_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    assert_annotated_linearized_matches_qpdf(
        &[
            ANNOTATION_ORDER_FOREIGN,
            "--pages",
            ANNOTATION_ORDER_FOREIGN,
            "1",
            ANNOTATION_ORDER_FXO,
            "1",
            "--",
        ],
        "annotated multi-source linearization must match qpdf",
    );
}

#[test]
fn annotated_duplicate_multi_source_linearize_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    assert_annotated_linearized_matches_qpdf(
        &[
            ANNOTATION_ORDER_FOREIGN,
            "--pages",
            ANNOTATION_ORDER_FOREIGN,
            "1",
            ANNOTATION_ORDER_FXO,
            "1",
            ANNOTATION_ORDER_FOREIGN,
            "1",
            "--",
        ],
        "annotated duplicate multi-source linearization must match qpdf",
    );
}

#[test]
fn multi_source_pages_generated_objstm_members_match_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf = run_qpdf(&qpdf_output);
    assert!(
        qpdf.status.success(),
        "qpdf probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--object-streams=generate",
            PRIMARY,
            "--pages",
            PRIMARY,
            "1",
            FOREIGN,
            "--",
        ])
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "multi-source generated ObjStm member order must match qpdf"
    );
}

#[test]
fn duplicate_page_generated_objstm_members_match_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf = run_qpdf_duplicate_page(&qpdf_output);
    assert!(
        qpdf.status.success(),
        "qpdf duplicate-page probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--newline-before-endstream=n",
            "--object-streams=generate",
            DUPLICATE_PRIMARY,
            "--pages",
            DUPLICATE_PRIMARY,
            "1,1",
            DUPLICATE_FOREIGN,
            "1",
            "--",
        ])
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "duplicate-page generated ObjStm allocation order must match qpdf"
    );
}

#[test]
fn duplicate_page_after_foreign_generated_objstm_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    assert_duplicate_after_foreign_matches_qpdf(
        &["--newline-before-endstream=n", "--object-streams=generate"],
        "duplicate page after a foreign source must preserve qpdf occurrence-order ObjStm members",
    );
}

#[test]
fn duplicate_page_after_foreign_qdf_generated_objstm_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    assert_duplicate_after_foreign_matches_qpdf(
        &["--qdf", "--object-streams=generate"],
        "duplicate page after a foreign source must preserve qpdf QDF ObjStm provenance",
    );
}

#[test]
fn duplicate_page_after_foreign_linearized_generated_objstm_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    assert_duplicate_after_foreign_matches_qpdf(
        &[
            "--newline-before-endstream=n",
            "--object-streams=generate",
            "--linearize",
        ],
        "duplicate page after a foreign source must preserve qpdf linearized ObjStm ordering",
    );
}

#[test]
fn duplicate_page_after_foreign_linearized_hint_stream_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let page_args = [
        OCCURRENCE_ORDER_PRIMARY,
        "--pages",
        OCCURRENCE_ORDER_PRIMARY,
        "1",
        OCCURRENCE_ORDER_FOREIGN,
        "1",
        OCCURRENCE_ORDER_PRIMARY,
        "1",
        "--",
    ];
    let mut qpdf_args = vec!["--static-id", "--linearize"];
    qpdf_args.extend_from_slice(&page_args);
    let qpdf = ProcessCommand::new("qpdf")
        .args(qpdf_args)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf linearized hint probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(["--static-id", "--linearize"])
        .args(page_args)
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized hint stream must preserve qpdf page-offset fields"
    );
}

#[test]
fn annotated_page_replay_preserves_qpdf_occurrence_provenance() {
    if skip_if_qpdf_missing() {
        return;
    }
    for (name, page_args) in [
        (
            "primary-foreign-duplicate",
            vec![
                ANNOTATION_ORDER_PRIMARY,
                "--pages",
                ANNOTATION_ORDER_PRIMARY,
                "1,2",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                ANNOTATION_ORDER_PRIMARY,
                "1",
                "--",
            ],
        ),
        (
            "annotation-link-annotation",
            vec![
                ANNOTATION_ORDER_FOREIGN,
                "--pages",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                ANNOTATION_ORDER_LINK,
                "1",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                "--",
            ],
        ),
        (
            "annotation-fxo-annotation",
            vec![
                ANNOTATION_ORDER_FOREIGN,
                "--pages",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                ANNOTATION_ORDER_FXO,
                "1",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                "--",
            ],
        ),
        (
            "annotation-two-annotation-two",
            vec![
                ANNOTATION_ORDER_PRIMARY,
                "--pages",
                ANNOTATION_ORDER_PRIMARY,
                "1",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                ANNOTATION_ORDER_PRIMARY,
                "1",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                "--",
            ],
        ),
        (
            "annotation-direct-dr-annotation",
            vec![
                ANNOTATION_ORDER_FOREIGN,
                "--pages",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                ANNOTATION_ORDER_DIRECT_DR,
                "1",
                ANNOTATION_ORDER_FOREIGN,
                "1",
                "--",
            ],
        ),
    ] {
        assert_annotated_replay_matches_qpdf(name, &page_args);
    }
}

fn assert_annotated_replay_matches_qpdf(name: &str, page_args: &[&str]) {
    assert_annotated_replay_with_flags(name, &["--static-id", "--qdf"], page_args);
}

#[test]
fn annotated_direct_dr_replay_preserves_qpdf_objstm_provenance() {
    if skip_if_qpdf_missing() {
        return;
    }
    let page_args = [
        ANNOTATION_ORDER_FOREIGN,
        "--pages",
        ANNOTATION_ORDER_FOREIGN,
        "1",
        ANNOTATION_ORDER_DIRECT_DR,
        "1",
        ANNOTATION_ORDER_FOREIGN,
        "1",
        "--",
    ];
    for (name, flags) in [
        (
            "annotation-direct-dr-objstm",
            vec!["--static-id", "--qdf", "--object-streams=generate"],
        ),
        (
            "annotation-direct-dr-linearized",
            vec![
                "--static-id",
                "--qdf",
                "--object-streams=generate",
                "--linearize",
            ],
        ),
    ] {
        assert_annotated_replay_with_flags(name, &flags, &page_args);
    }
}

// These are full-byte QDF gates because qpdf's repeated-page path allocates a
// shallow copy before later foreign-page objects, and QPDFWriter exposes that
// allocation provenance as `%% Original object ID` (QPDFJob.cc:2533-2538;
// QPDFWriter.cc:1773-1787).
#[test]
fn duplicate_page_before_foreign_qdf_provenance_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let page_args = [
        OCCURRENCE_ORDER_PRIMARY,
        "--pages",
        ".",
        "1,1",
        OCCURRENCE_ORDER_FOREIGN,
        "1",
        "--",
    ];
    assert_annotated_replay_with_flags(
        "duplicate-before-foreign-qdf",
        &["--static-id", "--qdf"],
        &page_args,
    );
}

#[test]
fn qtest_26_duplicate_page_qdf_provenance_matches_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let page_args = [
        PRIMARY, "--pages", ".", "3,2,3", ".", "2", FOREIGN, "1,1", FOREIGN, "1", "--",
    ];
    assert_annotated_replay_with_flags(
        "qtest-26-duplicate-page-qdf",
        &["--static-id", "--qdf"],
        &page_args,
    );
}

fn assert_annotated_replay_with_flags(name: &str, flags: &[&str], page_args: &[&str]) {
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let mut qpdf_args = flags.to_vec();
    qpdf_args.extend_from_slice(page_args);
    let qpdf = ProcessCommand::new("qpdf")
        .args(qpdf_args)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf annotated replay probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args(flags)
        .args(page_args)
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "annotation replay must preserve qpdf's occurrence allocation provenance for {name}"
    );
}

fn run_qpdf_linearized_merge(output: &Path) -> Output {
    ProcessCommand::new("qpdf")
        .args([
            "--static-id",
            "--newline-before-endstream=n",
            "--object-streams=generate",
            "--linearize",
            LINEARIZED_PRIMARY,
            "--pages",
            LINEARIZED_PRIMARY,
            "1",
            LINEARIZED_FOREIGN,
            "1",
            "--",
        ])
        .arg(output)
        .output()
        .expect("qpdf should spawn")
}

#[test]
fn linearized_multi_source_generated_objstm_members_match_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let qpdf_output = temp.path().join("qpdf.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");

    let qpdf = run_qpdf_linearized_merge(&qpdf_output);
    assert!(
        qpdf.status.success(),
        "qpdf linearized multi-source probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    Command::cargo_bin("flpdf")
        .unwrap()
        .args([
            "--static-id",
            "--newline-before-endstream=n",
            "--object-streams=generate",
            "--linearize",
            LINEARIZED_PRIMARY,
            "--pages",
            LINEARIZED_PRIMARY,
            "1",
            LINEARIZED_FOREIGN,
            "1",
            "--",
        ])
        .arg(&flpdf_output)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(&flpdf_output).unwrap(),
        std::fs::read(&qpdf_output).unwrap(),
        "linearized multi-source generated ObjStm order must match qpdf"
    );
}

#[test]
fn qdf_and_normalize_preserve_multi_source_objstm_bytes_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();

    for (label, extra_flags) in [
        ("default", Vec::new()),
        ("qdf", vec!["--qdf"]),
        ("normalize", vec!["--normalize-content=y"]),
    ] {
        let qpdf_output = temp.path().join(format!("qpdf-{label}.pdf"));
        let flpdf_output = temp.path().join(format!("flpdf-{label}.pdf"));
        let mut arguments = vec!["--static-id", "--newline-before-endstream=n"];
        arguments.extend(extra_flags);
        arguments.extend([QDF_PRIMARY, "--pages", ".", "1", QDF_FOREIGN, "1", "--"]);

        let qpdf = ProcessCommand::new("qpdf")
            .args(&arguments)
            .arg(&qpdf_output)
            .output()
            .expect("qpdf should spawn");
        assert!(
            qpdf.status.success(),
            "qpdf {label} probe failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        Command::cargo_bin("flpdf")
            .unwrap()
            .args(&arguments)
            .arg(&flpdf_output)
            .assert()
            .success();

        let qpdf_bytes = std::fs::read(&qpdf_output).unwrap();
        let flpdf_bytes = std::fs::read(&flpdf_output).unwrap();
        assert_eq!(
            qpdf_bytes
                .windows(b"/Type /ObjStm".len())
                .filter(|window| *window == b"/Type /ObjStm")
                .count(),
            1,
            "qpdf {label} probe must preserve the source ObjStm"
        );
        assert_eq!(
            flpdf_bytes
                .windows(b"/Type /ObjStm".len())
                .filter(|window| *window == b"/Type /ObjStm")
                .count(),
            1,
            "flpdf {label} probe must preserve the source ObjStm"
        );
        assert_eq!(
            flpdf_bytes, qpdf_bytes,
            "flpdf {label} QDF/content-normalization output must match qpdf byte-for-byte"
        );
    }
}

/// qpdf drops every source XRef stream from the writer queue in QDF mode
/// before it is numbered: `enqueueObject` returns early for
/// `isStreamOfType("/XRef")` so fix-qdf sees exactly one XRef stream, and its
/// comment names this case — a QDF built from a file with object streams while
/// preserving unreferenced objects (`QPDFWriter.cc:1085-1093`).
#[test]
fn qdf_drops_the_source_xref_stream_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let arguments = [
        "--static-id",
        "--newline-before-endstream=n",
        "--qdf",
        "--preserve-unreferenced",
        "--object-streams=disable",
        QDF_PRIMARY,
    ];

    let qpdf_output = temp.path().join("qpdf.pdf");
    let qpdf = ProcessCommand::new("qpdf")
        .args(arguments)
        .arg(&qpdf_output)
        .output()
        .expect("qpdf should spawn");
    assert!(
        qpdf.status.success(),
        "qpdf probe failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let flpdf_output = temp.path().join("flpdf.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(arguments)
        .arg(&flpdf_output)
        .assert()
        .success();

    for (label, path) in [("qpdf", &qpdf_output), ("flpdf", &flpdf_output)] {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(
            bytes
                .windows(b"/Type /XRef".len())
                .filter(|window| *window == b"/Type /XRef")
                .count(),
            0,
            "{label} must not carry a source XRef stream in QDF output"
        );
    }
}

/// qpdf's ADBE arbitration lives in the generic dictionary path guarded by
/// `is_root`, not by output mode — its own trace point passes
/// `m->qdf_mode ? 0 : 1` because the branch runs in both
/// (`QPDFWriter.cc:1396-1436`). A forced version therefore has to produce the
/// same `/Extensions /ADBE` under `--qdf` as without it.
#[test]
fn qdf_root_keeps_the_adbe_arbitration_like_qpdf() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();

    for object_streams in ["preserve", "generate"] {
        let arguments = [
            "--static-id",
            "--newline-before-endstream=n",
            "--qdf",
            "--force-version=1.8.5",
            &format!("--object-streams={object_streams}"),
            QDF_PRIMARY,
        ]
        .map(String::from);

        let qpdf_output = temp.path().join(format!("qpdf-{object_streams}.pdf"));
        let qpdf = ProcessCommand::new("qpdf")
            .args(&arguments)
            .arg(&qpdf_output)
            .output()
            .expect("qpdf should spawn");
        assert!(
            qpdf.status.success(),
            "qpdf probe failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        let flpdf_output = temp.path().join(format!("flpdf-{object_streams}.pdf"));
        Command::cargo_bin("flpdf")
            .unwrap()
            .args(&arguments)
            .arg(&flpdf_output)
            .assert()
            .success();

        let qpdf_bytes = std::fs::read(&qpdf_output).unwrap();
        let flpdf_bytes = std::fs::read(&flpdf_output).unwrap();
        for (label, bytes) in [("qpdf", &qpdf_bytes), ("flpdf", &flpdf_bytes)] {
            assert!(
                bytes
                    .windows(b"/ExtensionLevel 5".len())
                    .any(|window| window == b"/ExtensionLevel 5"),
                "{label} must arbitrate /ADBE for a forced extension level \
                 with --object-streams={object_streams}"
            );
        }
        assert_eq!(
            flpdf_bytes, qpdf_bytes,
            "QDF output with a forced extension level must match qpdf byte for byte \
             with --object-streams={object_streams}"
        );
    }
}
