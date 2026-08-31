use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

#[test]
fn test_renumber_usage_matches_qpdf_status_two() {
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "Usage: test_renumber [OPTION] INPUT.pdf\nOption:\n  --object-streams=preserve|disable|generate\n  --linearize\n  --preserve-unreferenced\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn test_renumber_rejects_unknown_options_and_missing_input() {
    let minimal = fixture("minimal.pdf");
    let minimal = minimal.to_str().expect("fixture path must be UTF-8");
    for args in [
        vec!["--not-an-option", minimal],
        vec!["--object-streams=bad", minimal],
        vec!["/path/that/does/not/exist.pdf"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
            .args(args)
            .output()
            .expect("test_renumber must be runnable");
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }

    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .arg("--linearize")
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "open : No such file or directory\n"
    );
}

#[test]
fn test_renumber_minimal_pdf_reports_success() {
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .arg(fixture("minimal.pdf"))
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--- compare between input and renumbered objects ---"));
    assert!(stdout.contains("--- compare between written and reloaded xref tables ---"));
    assert!(stdout.ends_with("succeeded\n"));
    assert!(output.stderr.is_empty());
}

#[test]
fn test_renumber_signed_qpdf_fixture_preserve_linearization_reports_success() {
    let Some(input) = std::env::var_os("FLPDF_QPDF_RENUMBER_INPUT") else {
        eprintln!("skipping signed qpdf fixture regression: env is unavailable");
        return;
    };
    let output = Command::new(env!("CARGO_BIN_EXE_test_renumber"))
        .args([
            "--linearize",
            input.to_str().expect("fixture path must be UTF-8"),
        ])
        .output()
        .expect("test_renumber must be runnable");
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&output.stdout).ends_with("succeeded\n"));
    assert!(output.stderr.is_empty());
}
