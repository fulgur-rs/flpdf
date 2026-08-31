//! qpdf 11.9.0 progress-reporting CLI contracts.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn progress_lines(output_name: &str, percentages: &[u8]) -> String {
    percentages
        .iter()
        .map(|percent| format!("flpdf: {output_name}: write progress: {percent}%\n"))
        .collect()
}

#[test]
fn progress_reports_file_output_on_info_stream() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("progress output is UTF-8"),
        progress_lines(
            &output_path.display().to_string(),
            &[0, 29, 43, 58, 72, 86, 99, 100]
        )
    );
    assert!(output.stderr.is_empty());
    assert!(output_path.exists(), "progress write must create the PDF");
}

#[test]
fn progress_reaches_the_attachment_writer() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let attachment_path = tempdir.path().join("payload.txt");
    std::fs::write(&attachment_path, b"attachment payload").expect("attachment");
    let output_path = tempdir.path().join("attachment-out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id", "--add-attachment"])
        .arg(&attachment_path)
        .arg("--")
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("progress output is UTF-8");
    assert!(
        stdout.starts_with(&format!(
            "flpdf: {}: write progress: 0%\n",
            output_path.display()
        )),
        "{stdout}"
    );
    assert!(stdout.contains("write progress: 100%\n"), "{stdout}");
    assert!(stdout
        .lines()
        .all(|line| line.contains(&output_path.display().to_string())));
    assert!(output.stderr.is_empty());
    assert!(
        output_path.exists(),
        "attachment rewrite must create the PDF"
    );
}

#[test]
fn progress_keeps_pdf_on_stdout_and_reports_on_stderr() {
    let input = fixture("one-page.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .arg("-")
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.starts_with(b"%PDF-1.3\n"));
    assert_eq!(
        String::from_utf8(output.stderr).expect("progress output is UTF-8"),
        progress_lines("standard output", &[0, 29, 43, 58, 72, 86, 99, 100])
    );
}

#[test]
fn native_rewrite_progress_uses_the_same_reporter_route() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("rewrite-out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["rewrite", "--progress", "--deterministic-id"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("progress output is UTF-8"),
        progress_lines(
            &output_path.display().to_string(),
            &[0, 29, 43, 58, 72, 86, 99, 100]
        )
    );
    assert!(output.stderr.is_empty());
    assert!(output_path.exists(), "progress rewrite must create the PDF");
}

#[test]
fn progress_warning_keeps_exit_three_and_completed_output() {
    let input = fixture_from_path("../../tests/fixtures/test_driver/repairable_input.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("warning-out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        String::from_utf8(output.stdout).expect("progress output is UTF-8"),
        progress_lines(&output_path.display().to_string(), &[0, 99, 100])
    );
    let stderr = String::from_utf8(output.stderr).expect("diagnostics are UTF-8");
    assert!(stderr.contains("file is damaged"), "stderr: {stderr}");
    assert!(
        stderr.contains("operation succeeded with warnings"),
        "stderr: {stderr}"
    );
    assert!(output_path.exists(), "warning exit must retain the output");
}

#[test]
fn progress_error_keeps_exit_two_without_progress_or_output() {
    let input = fixture_from_path("../../tests/fixtures/test_driver/open_repair_failure.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("error-out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("diagnostics are UTF-8");
    assert!(
        stderr.contains("unable to find trailer dictionary"),
        "stderr: {stderr}"
    );
    assert!(!output_path.exists(), "fatal input must not create output");
}

#[test]
fn progress_reports_each_split_writer_once() {
    let input = fixture("two-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("split.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id", "--split-pages=1"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("progress output is UTF-8");
    assert_eq!(
        stdout.matches("write progress: 0%\n").count(),
        2,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("write progress: 100%\n").count(),
        2,
        "{stdout}"
    );
    assert!(stdout
        .lines()
        .all(|line| line.contains(&output_path.display().to_string())));
    assert!(tempdir.path().join("split-1.pdf").exists());
    assert!(tempdir.path().join("split-2.pdf").exists());
    assert!(output.stderr.is_empty());
}

#[test]
fn progress_reports_each_pages_split_writer_once() {
    let input = fixture("two-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("pages-split.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .args(["--pages", ".", "1-2", "--", "--split-pages=1"])
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("progress output is UTF-8");
    assert_eq!(
        stdout.matches("write progress: 0%\n").count(),
        2,
        "{stdout}"
    );
    assert_eq!(
        stdout.matches("write progress: 100%\n").count(),
        2,
        "{stdout}"
    );
    assert!(stdout
        .lines()
        .all(|line| line.contains(&output_path.display().to_string())));
    assert!(tempdir.path().join("pages-split-1.pdf").exists());
    assert!(tempdir.path().join("pages-split-2.pdf").exists());
    assert!(output.stderr.is_empty());
}

#[test]
fn remove_attachment_keeps_progress_off_stdout() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let attachment_path = tempdir.path().join("payload.txt");
    std::fs::write(&attachment_path, b"attachment payload").expect("attachment");
    let with_attachment = tempdir.path().join("with-attachment.pdf");

    let add = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--deterministic-id", "--add-attachment"])
        .arg(&attachment_path)
        .arg("--key=rtkey")
        .arg("--")
        .arg(&input)
        .arg(&with_attachment)
        .output()
        .expect("flpdf invocation");
    assert!(
        add.status.success(),
        "add-attachment failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args([
            "--progress",
            "--deterministic-id",
            "--remove-attachment=rtkey",
        ])
        .arg(&with_attachment)
        .arg("-")
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Before this fix, `run_remove_attachment` never prepared standard-output
    // routing, so a literal "-" output path both fell through to being
    // treated as a real filename and left progress notifications free to
    // interleave with PDF bytes on stdout instead of stderr.
    assert!(output.stdout.starts_with(b"%PDF-1.3\n"));
    assert!(
        !output.stdout.windows(8).any(|window| window == b"progress"),
        "progress text leaked into PDF stdout bytes"
    );
    assert_eq!(
        String::from_utf8(output.stderr).expect("progress output is UTF-8"),
        progress_lines("standard output", &[0, 21, 31, 41, 51, 61, 71, 81, 100])
    );
    assert!(
        !tempdir.path().join("-").exists(),
        "stdout output must not create a file literally named \"-\""
    );
}

/// Build a classic-xref (no object streams) PDF with an embedded attachment
/// and then corrupt its `startxref` offset the same way
/// `tests/fixtures/test_driver/repairable_input.pdf` is corrupted, so
/// opening it triggers qpdf-shaped xref reconstruction warnings while still
/// carrying an attachment to remove.
fn damaged_pdf_with_attachment(dir: &Path, key: &str) -> PathBuf {
    let input = fixture("one-page.pdf");
    let attachment_path = dir.join("payload.txt");
    std::fs::write(&attachment_path, b"attachment payload").expect("attachment");
    let clean_path = dir.join("clean-with-attachment.pdf");

    let add = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--object-streams=disable", "--add-attachment"])
        .arg(&attachment_path)
        .arg(format!("--key={key}"))
        .arg("--")
        .arg(&input)
        .arg(&clean_path)
        .output()
        .expect("flpdf invocation");
    assert!(
        add.status.success(),
        "add-attachment failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let bytes = std::fs::read(&clean_path).expect("read clean pdf");
    let marker_start = rfind_bytes(&bytes, b"startxref\n").expect("startxref present");
    let eof_offset = rfind_bytes(&bytes, b"%%EOF").expect("%%EOF present");
    let mut damaged = bytes[..marker_start].to_vec();
    damaged.extend_from_slice(b"startxref\n0\n");
    damaged.extend_from_slice(&bytes[eof_offset..]);

    let damaged_path = dir.join("damaged-with-attachment.pdf");
    std::fs::write(&damaged_path, damaged).expect("write damaged pdf");
    damaged_path
}

fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

#[test]
fn remove_attachment_stdout_output_omits_the_resulting_file_warning_suffix() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let damaged = damaged_pdf_with_attachment(tempdir.path(), "repkey");

    let file_output_path = tempdir.path().join("removed-file.pdf");
    let file_output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .arg("--remove-attachment=repkey")
        .arg(&damaged)
        .arg(&file_output_path)
        .output()
        .expect("flpdf invocation");
    assert_eq!(file_output.status.code(), Some(3));
    let file_stderr = String::from_utf8(file_output.stderr).expect("diagnostics are UTF-8");
    assert!(
        file_stderr
            .contains("operation succeeded with warnings; resulting file may have some problems"),
        "stderr: {file_stderr}"
    );

    let stdout_output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .arg("--remove-attachment=repkey")
        .arg(&damaged)
        .arg("-")
        .output()
        .expect("flpdf invocation");
    assert_eq!(stdout_output.status.code(), Some(3));
    assert!(stdout_output.stdout.starts_with(b"%PDF-1.3\n"));
    let stdout_stderr = String::from_utf8(stdout_output.stderr).expect("diagnostics are UTF-8");
    assert!(
        stdout_stderr.contains("operation succeeded with warnings\n"),
        "stderr: {stdout_stderr}"
    );
    assert!(
        !stdout_stderr.contains("resulting file may have some problems"),
        "stdout output must not report a file-creation warning suffix: {stdout_stderr}"
    );
}

#[test]
fn progress_rotate_to_a_valid_destination_still_reports_progress() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_path = tempdir.path().join("rotate-out.pdf");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id", "--rotate=90:1"])
        .arg(&input)
        .arg(&output_path)
        .output()
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("progress output is UTF-8"),
        progress_lines(
            &output_path.display().to_string(),
            &[0, 29, 43, 58, 72, 86, 99, 100]
        )
    );
    assert!(output.stderr.is_empty());
    assert!(output_path.exists(), "the destination validation guard must leave a valid destination untouched and still let the rotate write succeed");
}

#[test]
fn progress_rewrite_to_unusable_destination_fails_before_any_progress_output() {
    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    // A directory is never a writable regular-file destination, matching the
    // qpdf oracle probe (`qpdf --progress --rotate=90:1 in.pdf a-directory`),
    // which fails immediately with no progress line printed at all.
    let bad_destination = tempdir.path().join("not-a-file");
    std::fs::create_dir(&bad_destination).expect("directory destination");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id", "--rotate=90:1"])
        .arg(&input)
        .arg(&bad_destination)
        .output()
        .expect("flpdf invocation");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "no progress output must be printed before the destination is validated: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("diagnostics are UTF-8");
    // The OS error text for opening a directory for writing is
    // platform-specific ("Is a directory" on Linux/macOS, "Access is denied"
    // on Windows); only the fail-fast-before-progress behavior above is the
    // qpdf-parity contract under test.
    assert!(!stderr.is_empty(), "expected a diagnostic on stderr");
}

// qpdf opens its real output destination exactly once. A destination
// validation probe that opens-then-closes a FIFO would let a blocking
// reader observe EOF and exit before the later real write reopens the
// pipe, so the real write would hang with no reader left (Unix-only:
// named pipes are not exercised on Windows).
#[cfg(unix)]
#[test]
fn progress_rotate_to_a_fifo_destination_does_not_hang() {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let fifo_path = tempdir.path().join("out.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .expect("mkfifo invocation")
            .success(),
        "mkfifo must succeed"
    );

    let reader_fifo = fifo_path.clone();
    let reader = std::thread::spawn(move || {
        let mut file = std::fs::File::open(&reader_fifo).expect("open fifo for reading");
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read fifo");
        buf
    });

    let child = std::process::Command::new(assert_cmd::cargo::cargo_bin("flpdf"))
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id", "--rotate=90:1"])
        .arg(&input)
        .arg(&fifo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flpdf");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let output = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("flpdf must not hang writing to a FIFO destination")
        .expect("flpdf invocation");

    assert!(
        output.status.success(),
        "flpdf failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bytes = reader.join().expect("fifo reader thread");
    assert!(
        bytes.starts_with(b"%PDF-1.3\n"),
        "fifo reader did not receive the rewritten PDF"
    );
}

// Unlike a FIFO, opening a Unix domain socket path as a plain file fails
// immediately (ENXIO) with no reader-synchronization hazard, so the
// destination-validation guard must still exempt only FIFOs and continue to
// fail fast here (Unix-only: this socket kind is not exercised on Windows).
#[cfg(unix)]
#[test]
fn progress_rotate_to_a_socket_destination_fails_before_any_progress_output() {
    use std::os::unix::net::UnixListener;

    let input = fixture("one-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let socket_path = tempdir.path().join("out.sock");
    let _listener = UnixListener::bind(&socket_path).expect("bind unix socket");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id", "--rotate=90:1"])
        .arg(&input)
        .arg(&socket_path)
        .output()
        .expect("flpdf invocation");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "no progress output must be printed before the destination is validated: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("diagnostics are UTF-8");
    assert!(!stderr.is_empty(), "expected a diagnostic on stderr");
}

#[test]
fn progress_pages_extraction_to_unusable_destination_fails_before_any_progress_output() {
    let input = fixture("two-page.pdf");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let bad_destination = tempdir.path().join("not-a-file");
    std::fs::create_dir(&bad_destination).expect("directory destination");

    let output = Command::cargo_bin("flpdf")
        .expect("flpdf binary")
        .env("FLPDF_STATIC_ID_QUIET", "1")
        .args(["--progress", "--deterministic-id"])
        .arg(&input)
        .args(["--pages", ".", "1", "--"])
        .arg(&bad_destination)
        .output()
        .expect("flpdf invocation");

    assert!(!output.status.success());
    assert!(
        output.stdout.is_empty(),
        "no progress output must be printed before the destination is validated: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8(output.stderr).expect("diagnostics are UTF-8");
    // The OS error text for opening a directory for writing is
    // platform-specific ("Is a directory" on Linux/macOS, "Access is denied"
    // on Windows); only the fail-fast-before-progress behavior above is the
    // qpdf-parity contract under test.
    assert!(!stderr.is_empty(), "expected a diagnostic on stderr");
}

fn fixture_from_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}
