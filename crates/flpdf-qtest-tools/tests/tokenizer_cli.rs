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
fn tokenizer_finds_endstream_without_preceding_delimiter() {
    // Regression test: qpdf's own endstream search (test_tokenizer.cc's
    // try_skipping/Finder) tokenizes forward from each literal "endstream"
    // match and never inspects the preceding byte, so stream data that
    // abuts "endstream" with no separating newline still matches.
    let dir = tempfile::tempdir().expect("create tempdir");
    let pdf_bytes: &[u8] =
        b"%PDF-1.4\n1 0 obj\n<< /Length 5 >>\nstream\nABCDEendstream\nendobj\n%%EOF\n";
    fs::write(dir.path().join("glued.pdf"), pdf_bytes).expect("write glued.pdf into tempdir");

    let output = run(&["glued.pdf"], dir.path());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("word: endstream"),
        "expected a standalone endstream token, got: {stdout}"
    );
    assert!(
        !stdout.contains("endstream not found"),
        "endstream search should not fail when stream data abuts endstream: {stdout}"
    );
}

#[test]
fn tokenizer_emits_repair_warnings_to_stderr() {
    // Regression test: repair diagnostics accumulated while reconstructing a
    // damaged file's xref table were collected but never surfaced. qpdf's
    // own processFile prints these warnings as it repairs the file.
    let dir = tempfile::tempdir().expect("create tempdir");
    let bytes = fs::read(
        fixture_dir()
            .join("test_driver")
            .join("repairable_input.pdf"),
    )
    .expect("read repairable_input.pdf fixture");
    fs::write(dir.path().join("repairable_input.pdf"), bytes)
        .expect("write repairable_input.pdf into tempdir");

    let output = run(&["repairable_input.pdf"], dir.path());

    assert!(
        output.status.success(),
        "unexpected exit status: {:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WARNING: repairable_input.pdf: file is damaged"));
    assert!(stderr.contains("WARNING: repairable_input.pdf: can't find startxref"));
    assert!(stderr.contains(
        "WARNING: repairable_input.pdf: Attempting to reconstruct cross-reference table"
    ));
}

fn build_pdf_with_page_content(content: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<u64> = vec![0];
    let push_obj = |out: &mut Vec<u8>, offsets: &mut Vec<u64>, body: &[u8]| {
        let n = offsets.len();
        offsets.push(out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    push_obj(&mut out, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    push_obj(
        &mut out,
        &mut offsets,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    );
    push_obj(
        &mut out,
        &mut offsets,
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /MediaBox [0 0 1 1] >>",
    );
    let mut stream_obj = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream_obj.extend_from_slice(content);
    stream_obj.extend_from_slice(b"\nendstream");
    push_obj(&mut out, &mut offsets, &stream_obj);

    let xref_start = out.len() as u64;
    let total = offsets.len();
    out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets[1..] {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

fn build_pdf_with_duplicate_page_leaf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<u64> = vec![0];
    let push_obj = |out: &mut Vec<u8>, offsets: &mut Vec<u64>, body: &[u8]| {
        let n = offsets.len();
        offsets.push(out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    push_obj(&mut out, &mut offsets, b"<< /Type /Catalog /Pages 2 0 R >>");
    // The same leaf, 3 0 R, appears twice in /Kids -- qpdf's getAllPages()
    // clones the second occurrence rather than deduplicating it.
    push_obj(
        &mut out,
        &mut offsets,
        b"<< /Type /Pages /Kids [3 0 R 3 0 R] /Count 2 >>",
    );
    push_obj(
        &mut out,
        &mut offsets,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 1 1] >>",
    );

    let xref_start = out.len() as u64;
    let total = offsets.len();
    out.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets[1..] {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

#[test]
fn tokenizer_repairs_page_tree_and_clones_duplicate_leaf() {
    // Regression test: qpdf's test_tokenizer.cc enumerates pages via
    // QPDFPageDocumentHelper(qpdf).getAllPages(), which is QPDF::getAllPages()'s
    // tree-repairing walk. A /Kids array that lists the same leaf twice must
    // therefore still produce two PAGE sections (the second is a clone), not
    // be silently deduplicated down to one.
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(
        dir.path().join("duplicate_leaf.pdf"),
        build_pdf_with_duplicate_page_leaf(),
    )
    .expect("write duplicate_leaf.pdf into tempdir");

    let output = run(&["duplicate_leaf.pdf"], dir.path());

    assert!(
        output.status.success(),
        "unexpected exit status: {:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--- BEGIN PAGE 1 ---"),
        "expected PAGE 1, got: {stdout}"
    );
    assert!(
        stdout.contains("--- BEGIN PAGE 2 ---"),
        "expected PAGE 2 for the cloned duplicate leaf, got: {stdout}"
    );
}

#[test]
fn tokenizer_recovers_inline_image_missing_id_separator() {
    // Regression test: when a page's content stream ends immediately after
    // `ID` (no separator byte), qpdf's is->read(&ch, 1) still proceeds to
    // expectInlineImage and records the cursor. flpdf must do the same
    // rather than silently skipping the recovery attempt.
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(
        dir.path().join("id_at_eof.pdf"),
        build_pdf_with_page_content(b"ID"),
    )
    .expect("write id_at_eof.pdf into tempdir");

    let output = run(&["id_at_eof.pdf"], dir.path());

    assert!(
        output.status.success(),
        "unexpected exit status: {:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("EI not found; resuming normal scanning"),
        "expected inline-image recovery output, got: {stdout}"
    );
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
fn tokenizer_maxlen_consumes_decimal_prefix() {
    // qpdf's -maxlen parsing goes through QUtil::string_to_uint, which runs the
    // argument through strtoull: a decimal prefix is consumed and anything
    // trailing the last digit is ignored, rather than str::parse's
    // all-or-nothing conversion rejecting the whole argument.
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(dir.path().join("minimal.pdf"), minimal_pdf_bytes())
        .expect("write minimal.pdf into tempdir");

    let output = run(&["-maxlen", "5trailing", "minimal.pdf"], dir.path());

    assert!(
        output.status.success(),
        "unexpected exit status: {:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
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
    // qpdf's FileInputSource open failure reports "open <filename>: <error>";
    // the message must name the file, not just the bare OS error text.
    assert!(
        stderr.contains("open nonexistent.pdf:"),
        "expected the filename in the open error, got: {stderr}"
    );
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
