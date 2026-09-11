//! qpdf 11.9.0 allocation-error parity for materialized page ranges.

#![cfg(all(feature = "qpdf-zlib-compat", unix))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE: &str = "tests/fixtures/compat/three-page.pdf";
const RANGE: &str = "1-2147483647";
const VIRTUAL_MEMORY_LIMIT_KIB: &str = "2000000";

fn qpdf_is_available() -> bool {
    let version = Command::new("qpdf").arg("--version").output().ok();
    if version.as_ref().is_some_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).lines().next() == Some("qpdf version 11.9.0")
    }) {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for allocation-error parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    false
}

fn low_memory_script(directory: &Path) -> PathBuf {
    let script = directory.join("run-with-low-memory.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nulimit -v {VIRTUAL_MEMORY_LIMIT_KIB}\nbinary=$1\nshift\nexec \"$binary\" \"$@\"\n"
        ),
    )
    .expect("write low-memory wrapper");
    script
}

fn run_with_low_memory(directory: &Path, script: &Path, binary: &Path, args: &[String]) -> Output {
    Command::new("/bin/sh")
        .current_dir(directory)
        .arg(script)
        .arg(binary)
        .args(args)
        .output()
        .expect("low-memory process invocation")
}

fn page_args(output: &Path) -> Vec<String> {
    vec![
        "--static-id".to_owned(),
        "--qdf".to_owned(),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(FIXTURE)
            .display()
            .to_string(),
        "--pages".to_owned(),
        ".".to_owned(),
        RANGE.to_owned(),
        "--".to_owned(),
        output.display().to_string(),
    ]
}

#[test]
fn oversized_page_range_reports_qpdf_bad_alloc_as_exit_two() {
    if !qpdf_is_available() {
        return;
    }
    let directory = tempfile::tempdir().expect("temporary allocation-error directory");
    let script = low_memory_script(directory.path());
    let qpdf_output = directory.path().join("qpdf.pdf");
    let flpdf_output = directory.path().join("flpdf.pdf");
    let qpdf = run_with_low_memory(
        directory.path(),
        &script,
        Path::new("qpdf"),
        &page_args(&qpdf_output),
    );
    assert_eq!(
        qpdf.status.code(),
        Some(2),
        "qpdf must report bad_alloc: {qpdf:?}"
    );
    assert_eq!(qpdf.stdout, b"");
    assert_eq!(qpdf.stderr, b"qpdf: std::bad_alloc\n");

    let flpdf = run_with_low_memory(
        directory.path(),
        &script,
        Path::new(env!("CARGO_BIN_EXE_flpdf")),
        &page_args(&flpdf_output),
    );
    assert_eq!(
        flpdf.status.code(),
        Some(2),
        "flpdf must report allocation failure as a qpdf error: {flpdf:?}"
    );
    assert_eq!(flpdf.stdout, b"");
    assert_eq!(flpdf.stderr, b"flpdf: std::bad_alloc\n");
    assert!(!qpdf_output.exists(), "qpdf must not write a failed output");
    assert!(
        !flpdf_output.exists(),
        "flpdf must not write a failed output"
    );
}
