use assert_cmd::Command;
use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Command as StdCommand, Stdio};

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

fn run_merged(args: &[&str], cwd: &std::path::Path) -> (std::process::ExitStatus, Vec<u8>) {
    let merged = tempfile::tempfile().expect("create merged output file");
    let stdout = merged.try_clone().expect("clone merged output file");
    let stderr = merged.try_clone().expect("clone merged output file");
    let status = StdCommand::new(assert_cmd::cargo_bin!("flpdf-test-tokenizer"))
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .expect("spawn flpdf-test-tokenizer");

    let mut merged = merged;
    merged
        .seek(SeekFrom::Start(0))
        .expect("rewind merged output file");
    let mut output = Vec::new();
    merged
        .read_to_end(&mut output)
        .expect("read merged output file");
    (status, output)
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

#[test]
fn tokenizer_emits_canonical_stream_recovery_warnings_once_with_qpdf_offsets() {
    // The fixture contains malformed stream lengths in both direct and object
    // stream objects. Resolve each canonical handle before crossing into the
    // legacy decode boundary: qpdf emits one warning sequence per object, and
    // its /Length warning offset is the post-obj header position.
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = fixture_dir()
        .join("compat")
        .join("null-length-framing-matrix-objstm.pdf");
    fs::write(
        dir.path().join("null-length-framing-matrix-objstm.pdf"),
        fs::read(fixture).expect("read malformed stream fixture"),
    )
    .expect("write malformed stream fixture into tempdir");

    let output = run(&["null-length-framing-matrix-objstm.pdf"], dir.path());

    assert!(
        output.status.success(),
        "unexpected exit status: {:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning_lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("WARNING:"))
        .collect();
    assert_eq!(
        warning_lines.len(),
        36,
        "unexpected warning output: {stderr}"
    );
    assert_eq!(
        warning_lines.iter().collect::<HashSet<_>>().len(),
        warning_lines.len(),
        "canonical and legacy stream reads must not duplicate warnings: {stderr}"
    );
    assert!(
        warning_lines.iter().any(|line| {
            line.contains("(object 5 0, offset 76): stream dictionary lacks /Length key")
        }),
        "qpdf's post-header offset must be preserved: {stderr}"
    );
    assert!(
        !warning_lines.iter().any(|line| {
            line.contains("(object 5 0, offset 69): stream dictionary lacks /Length key")
        }),
        "the xref/object-start offset must not replace qpdf's readObject offset: {stderr}"
    );
}

#[test]
fn tokenizer_decodes_objstm_from_canonical_handle_without_replaying_container_recovery() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let fixture = fixture_dir()
        .join("compat")
        .join("null-length-framing-matrix-objstm.pdf");
    let mut bytes = fs::read(fixture).expect("read ObjStm fixture");
    let length = b"/Length 213";
    let length_start = bytes
        .windows(length.len())
        .position(|window| window == length)
        .expect("ObjStm /Length marker");
    bytes[length_start..length_start + length.len()].fill(b' ');
    fs::write(dir.path().join("missing-objstm-length.pdf"), bytes)
        .expect("write malformed ObjStm fixture into tempdir");

    let output = run(&["missing-objstm-length.pdf"], dir.path());

    assert!(
        output.status.success(),
        "unexpected exit status: {:?}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning_lines: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("WARNING:"))
        .filter(|line| {
            line.contains("object 20 0") && line.contains("stream dictionary lacks /Length key")
        })
        .collect();
    assert_eq!(
        warning_lines.len(),
        1,
        "the ObjStm container must be recovered once through its canonical handle: {stderr}"
    );
    let container_warnings: Vec<_> = stderr
        .lines()
        .filter(|line| line.starts_with("WARNING:") && line.contains("object 20 0"))
        .collect();
    assert_eq!(
        container_warnings.len(),
        3,
        "the canonical ObjStm recovery sequence must contain exactly three warnings: {stderr}"
    );
    assert!(container_warnings[0].contains("offset 840"));
    assert!(container_warnings[1].contains("offset 916"));
    assert!(container_warnings[2].contains("offset 916"));
}

#[test]
fn tokenizer_flushes_objstm_decode_warning_before_propagating_error() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let mut bytes = fs::read(fixture_dir().join("test_driver/stream_flate_error.pdf"))
        .expect("read malformed Flate fixture");
    let marker = b"<< /Filter /FlateDecode /Length 3 >>";
    let replacement = b"<< /Type /ObjStm /Filter /FlateDecode /Length 3 >>";
    let marker_start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("stream dictionary marker");
    bytes.splice(
        marker_start..marker_start + marker.len(),
        replacement.iter().copied(),
    );
    fs::write(dir.path().join("malformed-objstm.pdf"), bytes)
        .expect("write malformed ObjStm fixture");

    let output = run(&["malformed-objstm.pdf"], dir.path());

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning = "error decoding stream data for object 6 0";
    let exception = "error getting decoded stream data";
    let warning_offset = stderr.find(warning).expect("ObjStm decode warning");
    let exception_offset = stderr.find(exception).expect("decode exception");
    assert!(
        warning_offset < exception_offset,
        "the object-specific warning must precede the generic exception: {stderr}"
    );
}

#[test]
fn tokenizer_flushes_objstm_success_warning_before_token_output() {
    let dir = tempfile::tempdir().expect("create tempdir");
    let mut bytes = fs::read(fixture_dir().join("test_driver/stream_flate_error.pdf"))
        .expect("read malformed filter fixture");
    let marker = b"<< /Filter /FlateDecode /Length 3 >>";
    let replacement = b"<< /Type /ObjStm /Filter 42 /Length 3 >>";
    let marker_start = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("stream dictionary marker");
    bytes.splice(
        marker_start..marker_start + marker.len(),
        replacement.iter().copied(),
    );
    fs::write(dir.path().join("unfilterable-objstm.pdf"), bytes)
        .expect("write unfilterable ObjStm fixture");

    let (status, output) = run_merged(&["unfilterable-objstm.pdf"], dir.path());

    assert!(status.success(), "unexpected exit status: {status:?}");
    let output = String::from_utf8_lossy(&output);
    let warning = "stream filter type is not name or array";
    let tokens = "--- BEGIN OBJECT STREAM 6 ---";
    let warning_offset = output.find(warning).expect("unfilterable filter warning");
    let token_offset = output.find(tokens).expect("ObjStm token output");
    assert!(
        warning_offset < token_offset,
        "the filter warning must precede ObjStm token output: {output}"
    );
}

#[test]
fn tokenizer_reuses_canonical_page_stream_resolution_without_replaying_warnings() {
    let cases = [
        (
            "chained-indirect-contents.pdf",
            "(object 5 0, offset 232): expected endobj",
            None,
        ),
        (
            "encrypted-recovered-eol.pdf",
            "(object 4 0, offset 236): stream dictionary lacks /Length key",
            Some("(object 4 0, offset 229): stream dictionary lacks /Length key"),
        ),
    ];

    for (filename, expected_warning, obsolete_warning) in cases {
        let dir = tempfile::tempdir().expect("create tempdir");
        let fixture = fixture_dir().join("compat").join(filename);
        fs::write(
            dir.path().join(filename),
            fs::read(&fixture).expect("read tokenizer warning fixture"),
        )
        .expect("write tokenizer warning fixture into tempdir");

        let output = run(&[filename], dir.path());
        assert!(
            output.status.success(),
            "unexpected exit status for {filename}: {:?}; stderr={:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.matches(expected_warning).count(),
            1,
            "canonical warning must be emitted once for {filename}: {stderr}"
        );
        if let Some(obsolete_warning) = obsolete_warning {
            assert!(
                !stderr.contains(obsolete_warning),
                "legacy page-content offset must not be emitted for {filename}: {stderr}"
            );
        }
        if filename == "encrypted-recovered-eol.pdf" {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("12343: word: Q"),
                "expected the recovered page content to end with Q: {stdout}"
            );
            assert!(
                stdout.contains("12345: word:"),
                "qpdf pipes the complete recovered encrypted span: {stdout}"
            );
            assert!(
                stdout.contains("12359: brace_close: }"),
                "qpdf's trailing content-parser token must be visible: {stdout}"
            );
            assert!(
                stdout.contains("12360: word:"),
                "qpdf's trailing word token must be visible: {stdout}"
            );
            assert!(
                stdout.contains("12368: eof"),
                "tokenizer must consume the complete recovered span: {stdout}"
            );
        }
    }
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

fn build_pdf_with_page_contents_value(value: &[u8]) -> Vec<u8> {
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
    let mut page = b"<< /Type /Page /Parent 2 0 R /Contents ".to_vec();
    page.extend_from_slice(value);
    page.extend_from_slice(b" /MediaBox [0 0 1 1] >>");
    push_obj(&mut out, &mut offsets, &page);

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

fn build_pdf_with_page_contents_array_cycle() -> Vec<u8> {
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
    // Object 4 resolves to an array that refers to itself. The collector must
    // stop on the active recursion path rather than growing the call stack
    // indefinitely while still allowing repeated non-cyclic references.
    push_obj(&mut out, &mut offsets, b"[4 0 R]");

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

fn build_pdf_with_nested_page_contents_array() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<u64> = vec![0];
    let push_obj = |out: &mut Vec<u8>, offsets: &mut Vec<u64>, body: &[u8]| {
        let n = offsets.len();
        offsets.push(out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    let push_stream = |out: &mut Vec<u8>, offsets: &mut Vec<u64>, content: &[u8]| {
        let body = format!("<< /Length {} >>\nstream\n", content.len());
        let mut stream = body.into_bytes();
        stream.extend_from_slice(content);
        stream.extend_from_slice(b"\nendstream");
        push_obj(out, offsets, &stream);
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
    push_obj(&mut out, &mut offsets, b"[5 0 R 6 0 R]");
    push_obj(&mut out, &mut offsets, b"[7 0 R]");
    push_stream(&mut out, &mut offsets, b"q\n");
    push_stream(&mut out, &mut offsets, b"Q\n");

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
fn tokenizer_bounds_recursive_page_contents_collection() {
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(
        dir.path().join("contents_cycle.pdf"),
        build_pdf_with_page_contents_array_cycle(),
    )
    .expect("write contents_cycle.pdf into tempdir");

    let output = run(&["contents_cycle.pdf"], dir.path());

    assert!(
        output.status.success(),
        "recursive /Contents must not abort tokenization: status={:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--- BEGIN PAGE 1 ---"),
        "expected the page section even when /Contents is cyclic: {stdout}"
    );
    assert!(
        !stdout.contains("word: q"),
        "a cyclic array must not fabricate a content stream: {stdout}"
    );
}

#[test]
fn tokenizer_skips_nested_page_content_arrays() {
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(
        dir.path().join("nested_contents.pdf"),
        build_pdf_with_nested_page_contents_array(),
    )
    .expect("write nested_contents.pdf into tempdir");

    let output = run(&["nested_contents.pdf"], dir.path());

    assert!(
        output.status.success(),
        "nested /Contents arrays must not abort tokenization: status={:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let page = stdout
        .split_once("--- BEGIN PAGE 1 ---")
        .and_then(|(_, rest)| rest.split_once("--- END PAGE 1 ---"))
        .map(|(page, _)| page)
        .expect("page token section");
    assert!(
        page.contains("word: q"),
        "the direct stream must remain in page contents: {page}"
    );
    assert!(
        !page.contains("word: Q"),
        "a nested array element must not be recursively flattened: {page}"
    );
}

#[test]
fn tokenizer_treats_null_page_contents_as_empty_without_fallback() {
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(
        dir.path().join("null_contents.pdf"),
        build_pdf_with_page_contents_value(b"null"),
    )
    .expect("write null_contents.pdf into tempdir");

    let output = run(&["null_contents.pdf"], dir.path());

    assert!(
        output.status.success(),
        "explicit null /Contents is an empty page: status={:?}; stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--- BEGIN PAGE 1 ---"),
        "expected page output: {stdout}"
    );
    assert!(
        stdout.contains("--- END PAGE 1 ---"),
        "expected empty page output: {stdout}"
    );
}

#[test]
fn tokenizer_propagates_non_null_page_contents_errors() {
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(
        dir.path().join("scalar_contents.pdf"),
        build_pdf_with_page_contents_value(b"17"),
    )
    .expect("write scalar_contents.pdf into tempdir");

    let output = run(&["scalar_contents.pdf"], dir.path());

    assert_eq!(
        output.status.code(),
        Some(2),
        "scalar /Contents must not be swallowed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a stream or array"),
        "expected the page-content error, got: {stderr}"
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
fn tokenizer_maxlen_negative_value_shows_usage() {
    // QUtil::string_to_uint rejects a leading '-' outright (an uncaught
    // exception in real qpdf); this helper falls back to usage() instead of
    // silently accepting a negative -maxlen value.
    let dir = tempfile::tempdir().expect("create tempdir");
    fs::write(dir.path().join("minimal.pdf"), minimal_pdf_bytes())
        .expect("write minimal.pdf into tempdir");

    let output = run(&["-maxlen", "-5", "minimal.pdf"], dir.path());

    assert_eq!(output.status.code(), Some(2));
    assert_stderr_contains_usage(&output);
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
