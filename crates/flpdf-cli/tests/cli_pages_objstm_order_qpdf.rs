//! qpdf 11.9.0 byte parity for multi-source pages with generated ObjStms.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
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
