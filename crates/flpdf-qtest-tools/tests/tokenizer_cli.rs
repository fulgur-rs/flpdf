use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests")
        .join("fixtures")
}

fn minimal_pdf_bytes() -> Vec<u8> {
    fs::read(fixture_dir().join("minimal.pdf")).expect("read minimal.pdf fixture")
}

fn run(args: &[&str], cwd: &std::path::Path) -> std::process::Output {
    Command::cargo_bin("flpdf-test-tokenizer")
        .unwrap()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn flpdf-test-tokenizer")
}

fn assert_stderr_contains_usage(output: &std::process::Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Usage: "),
        "expected Usage:, got: {stderr}"
    );
}

#[test]
fn tokenizer_tokens_minimal_pdf() {
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(dir.path().join("minimal.pdf"), minimal_pdf_bytes())
        .expect("write minimal.pdf into tempdir");

    let output = run(&["minimal.pdf"], dir.path());

    assert!(
        output.status.success(),
        "unexpected exit status: {:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.stderr.is_empty());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--- BEGIN FILE ---"));
    assert!(stdout.contains("--- END FILE ---"));
    assert!(stdout.contains("word: obj"));
    assert!(stdout.contains("eof"));
}

#[test]
fn tokenizer_no_ignorable_flag() {
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(dir.path().join("minimal.pdf"), minimal_pdf_bytes())
        .expect("write minimal.pdf into tempdir");

    let output = run(&["-no-ignorable", "minimal.pdf"], dir.path());

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--- BEGIN FILE ---"));
    assert!(!stdout.contains("space:"));
    assert!(!stdout.contains("comment:"));
}

#[test]
fn tokenizer_maxlen_flag() {
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(dir.path().join("minimal.pdf"), minimal_pdf_bytes())
        .expect("write minimal.pdf into tempdir");

    let output = run(&["-maxlen", "5", "minimal.pdf"], dir.path());

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("exceeded allowable length"));
}

#[test]
fn tokenizer_missing_file_exits_two() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let output = run(&["nonexistent.pdf"], dir.path());

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exception:"));
}

#[test]
fn tokenizer_missing_filename_shows_usage() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let output = run(&[], dir.path());

    assert_eq!(output.status.code(), Some(2));
    assert_stderr_contains_usage(&output);
}

#[test]
fn tokenizer_bad_option_shows_usage() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let output = run(&["--bad-flag", "minimal.pdf"], dir.path());

    assert_eq!(output.status.code(), Some(2));
    assert_stderr_contains_usage(&output);
}

#[test]
fn tokenizer_maxlen_missing_value_shows_usage() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let output = run(&["-maxlen"], dir.path());

    assert_eq!(output.status.code(), Some(2));
    assert_stderr_contains_usage(&output);
}

#[test]
fn tokenizer_two_filenames_shows_usage() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let output = run(&["a.pdf", "b.pdf"], dir.path());

    assert_eq!(output.status.code(), Some(2));
    assert_stderr_contains_usage(&output);
}
