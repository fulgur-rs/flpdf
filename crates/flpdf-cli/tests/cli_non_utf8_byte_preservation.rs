#![cfg(target_os = "linux")]

//! qpdf 11.9.0 raw-byte argv and report regressions.

use assert_cmd::cargo::cargo_bin;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const ONE_PAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/one-page.pdf"
);
const LINEARIZED_ONE_PAGE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/linearized-one-page.pdf"
);
const JSON_COMPLETE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/json-input/complete.json"
);

fn raw(value: &[u8]) -> OsString {
    OsString::from_vec(value.to_vec())
}

fn raw_path(directory: &Path, name: &[u8]) -> PathBuf {
    directory.join(raw(name))
}

fn run(args: impl IntoIterator<Item = OsString>) -> std::process::Output {
    Command::new(cargo_bin!("flpdf"))
        .env("FLPDF_PROGNAME", "qpdf")
        .args(args)
        .output()
        .expect("run flpdf")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn assert_preserves_raw_bytes(output: &std::process::Output, bytes: &[u8], context: &str) {
    assert!(
        contains_bytes(&output.stdout, bytes) || contains_bytes(&output.stderr, bytes),
        "{context}: raw bytes {:?} absent from stdout/stderr\nstdout={:?}\nstderr={:?}",
        bytes,
        output.stdout,
        output.stderr
    );
    assert!(
        !contains_bytes(&output.stdout, b"\xef\xbf\xbd")
            && !contains_bytes(&output.stderr, b"\xef\xbf\xbd"),
        "{context}: output contains U+FFFD\nstdout={:?}\nstderr={:?}",
        output.stdout,
        output.stderr
    );
}

#[test]
fn password_option_accepts_non_utf8_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from("--password"),
        raw(b"password-\xff"),
        OsString::from("--check"),
        input.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
}

#[test]
fn encrypt_segment_preserves_non_utf8_password_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let encrypted = raw_path(directory.path(), b"encrypted.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from("--allow-weak-crypto"),
        OsString::from("--encrypt"),
        raw(b"user-\xff"),
        OsString::from("owner"),
        OsString::from("128"),
        OsString::from("--"),
        input.into_os_string(),
        encrypted.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert!(encrypted.exists(), "encrypted output was not created");

    let check = run([
        OsString::from("--password-mode=bytes"),
        OsString::from("--password"),
        raw(b"user-\xff"),
        OsString::from("--check"),
        encrypted.into_os_string(),
    ]);
    assert_eq!(check.status.code(), Some(0), "stderr={:?}", check.stderr);
}

#[test]
fn encryption_file_password_accepts_non_utf8_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let donor = raw_path(directory.path(), b"donor.pdf");
    let copied = raw_path(directory.path(), b"copied.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let encrypt = run([
        OsString::from("--allow-weak-crypto"),
        OsString::from("--encrypt"),
        raw(b"user-\xff"),
        OsString::from("owner"),
        OsString::from("128"),
        OsString::from("--use-aes=y"),
        OsString::from("--"),
        input.clone().into_os_string(),
        donor.clone().into_os_string(),
    ]);
    assert_eq!(
        encrypt.status.code(),
        Some(0),
        "stderr={:?}",
        encrypt.stderr
    );

    let copy = run([
        OsString::from("--copy-encryption"),
        donor.into_os_string(),
        OsString::from_vec([b"--encryption-file-password=".as_slice(), b"user-\xff"].concat()),
        input.into_os_string(),
        copied.clone().into_os_string(),
    ]);
    assert_eq!(copy.status.code(), Some(0), "stderr={:?}", copy.stderr);

    let check = run([
        OsString::from("--password-mode=bytes"),
        OsString::from("--password"),
        raw(b"user-\xff"),
        OsString::from("--check"),
        copied.into_os_string(),
    ]);
    assert_eq!(check.status.code(), Some(0), "stderr={:?}", check.stderr);
}

#[test]
fn json_input_error_preserves_non_utf8_input_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"json-\xff.json");
    fs::copy(JSON_COMPLETE, &input).expect("copy JSON input");

    let output = run([
        OsString::from("--json-input"),
        OsString::from("--check"),
        input.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(&output, input.as_os_str().as_bytes(), "JSON input error");
}

#[test]
fn malformed_json_input_error_preserves_non_utf8_input_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"malformed-\xff.json");
    fs::write(&input, b"{}").expect("write malformed JSON input");

    let output = run([
        OsString::from("--json-input"),
        OsString::from("--check"),
        input.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        input.as_os_str().as_bytes(),
        "malformed JSON input error",
    );
}

#[test]
fn overlay_verbose_report_preserves_non_utf8_source_path() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let overlay = raw_path(directory.path(), b"overlay-\xff.pdf");
    let output_path = raw_path(directory.path(), b"output.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");
    fs::copy(ONE_PAGE, &overlay).expect("copy overlay");

    let output = run([
        OsString::from("--verbose"),
        input.into_os_string(),
        OsString::from("--overlay"),
        overlay.clone().into_os_string(),
        OsString::from("--"),
        output_path.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        overlay.as_os_str().as_bytes(),
        "overlay verbose report",
    );
}

#[test]
fn page_segment_password_accepts_non_utf8_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let source = raw_path(directory.path(), b"source.pdf");
    let output_path = raw_path(directory.path(), b"output.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");
    fs::copy(ONE_PAGE, &source).expect("copy source");

    let output = run([
        input.into_os_string(),
        OsString::from("--pages"),
        source.into_os_string(),
        OsString::from_vec([b"--password=".as_slice(), b"segment-\xff"].concat()),
        OsString::from("1"),
        OsString::from("--"),
        output_path.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
}

#[test]
fn overlay_segment_password_accepts_non_utf8_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let overlay = raw_path(directory.path(), b"overlay.pdf");
    let output_path = raw_path(directory.path(), b"output.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");
    fs::copy(ONE_PAGE, &overlay).expect("copy overlay");

    let output = run([
        input.into_os_string(),
        OsString::from("--overlay"),
        overlay.into_os_string(),
        OsString::from_vec([b"--password=".as_slice(), b"segment-\xff"].concat()),
        OsString::from("--"),
        output_path.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
}

#[test]
fn show_linearization_report_preserves_non_utf8_input_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"linearized-\xff.pdf");
    fs::copy(LINEARIZED_ONE_PAGE, &input).expect("copy linearized input");

    let output = run([
        OsString::from("--show-linearization"),
        input.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        input.as_os_str().as_bytes(),
        "show-linearization report",
    );
}

#[test]
fn linearization_parameter_warning_preserves_non_utf8_input_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"linearized-\xff.pdf");
    let mut bytes = fs::read(LINEARIZED_ONE_PAGE).expect("read linearized input");
    let marker = b"/N 1";
    let position = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("linearization /N marker");
    bytes[position..position + marker.len()].copy_from_slice(b"/N 2");
    fs::write(&input, bytes).expect("write malformed linearized input");

    let output = run([
        OsString::from("--show-linearization"),
        input.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(3), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        input.as_os_str().as_bytes(),
        "linearization warning",
    );
}

#[test]
fn attachment_lookup_options_preserve_non_utf8_key_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let payload = raw_path(directory.path(), b"payload.bin");
    let attached = raw_path(directory.path(), b"attached.pdf");
    let removed = raw_path(directory.path(), b"removed.pdf");
    let key = b"key-\xff";
    fs::copy(ONE_PAGE, &input).expect("copy input");
    fs::write(&payload, b"attachment payload").expect("write payload");

    let add = run([
        OsString::from("--add-attachment"),
        payload.into_os_string(),
        OsString::from_vec([b"--key=".as_slice(), key].concat()),
        OsString::from("--"),
        input.into_os_string(),
        attached.clone().into_os_string(),
    ]);
    assert_eq!(add.status.code(), Some(0), "stderr={:?}", add.stderr);

    let show = run([
        OsString::from_vec([b"--show-attachment=".as_slice(), key].concat()),
        attached.clone().into_os_string(),
    ]);
    assert_eq!(show.status.code(), Some(0), "stderr={:?}", show.stderr);
    assert_eq!(show.stdout, b"attachment payload");

    let remove = run([
        OsString::from_vec([b"--remove-attachment=".as_slice(), key].concat()),
        attached.into_os_string(),
        removed.into_os_string(),
    ]);
    assert_eq!(remove.status.code(), Some(0), "stderr={:?}", remove.stderr);
}

#[test]
fn missing_show_attachment_diagnostic_preserves_non_utf8_key_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let key = b"missing-\xff";
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from_vec([b"--show-attachment=".as_slice(), key].concat()),
        input.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(&output, key, "missing show-attachment diagnostic");
    let mut expected = b"qpdf: --show-attachment: key \"missing-".to_vec();
    expected.push(0xff);
    expected.extend_from_slice(
        b"\" not found or unreadable: unsupported PDF feature: attachment \"missing-",
    );
    expected.push(0xff);
    expected.extend_from_slice(b"\" not found\n");
    assert_eq!(output.stderr, expected);
}

#[test]
fn missing_remove_attachment_diagnostic_preserves_non_utf8_key_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let output_path = raw_path(directory.path(), b"output.pdf");
    let key = b"missing-\xff";
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from_vec([b"--remove-attachment=".as_slice(), key].concat()),
        input.into_os_string(),
        output_path.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(&output, key, "missing remove-attachment diagnostic");
    let mut expected = b"qpdf: --remove-attachment: key \"missing-".to_vec();
    expected.push(0xff);
    expected.extend_from_slice(b"\" not found in document\n");
    assert_eq!(output.stderr, expected);
}

#[test]
fn missing_show_attachment_diagnostic_escapes_control_bytes_in_the_inner_message() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let key = b"bad\nkey-\xff";
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from_vec([b"--show-attachment=".as_slice(), key].concat()),
        input.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    // The control byte must not split the single-line diagnostic in two: the
    // key's raw `\n` is escaped in both the outer CLI-level quoting and the
    // inner library-level "attachment ... not found" message.
    assert_eq!(
        output.stderr.iter().filter(|&&byte| byte == b'\n').count(),
        1,
        "stderr={:?}",
        output.stderr
    );
    let mut expected = b"qpdf: --show-attachment: key \"bad\\nkey-".to_vec();
    expected.push(0xff);
    expected.extend_from_slice(
        b"\" not found or unreadable: unsupported PDF feature: attachment \"bad\\nkey-",
    );
    expected.push(0xff);
    expected.extend_from_slice(b"\" not found\n");
    assert_eq!(output.stderr, expected);
}

#[test]
fn attachment_diagnostic_uses_byte_safe_debug_quoting() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let output_path = raw_path(directory.path(), b"output.pdf");
    let key = b"quote-\"-\n-\r-\t-\x01-\\-\xff";
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from_vec([b"--remove-attachment=".as_slice(), key].concat()),
        input.into_os_string(),
        output_path.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    let mut expected =
        b"qpdf: --remove-attachment: key \"quote-\\\"-\\n-\\r-\\t-\\x01-\\\\-".to_vec();
    expected.push(0xff);
    expected.extend_from_slice(b"\" not found in document\n");
    assert_eq!(output.stderr, expected);
}

#[test]
fn json_input_failure_preserves_non_utf8_input_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input-\xff.json");
    fs::write(&input, b"{").expect("write malformed JSON");

    let output = run([
        OsString::from("--json-input"),
        OsString::from("--json-output=2"),
        input.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(&output, input.as_os_str().as_bytes(), "JSON input failure");
}

#[test]
fn json_input_validation_failure_preserves_non_utf8_input_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"validation-\xff.json");
    fs::write(&input, b"{}").expect("write invalid complete JSON");

    let output = run([
        OsString::from("--json-input"),
        OsString::from("--json-output=2"),
        input.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        input.as_os_str().as_bytes(),
        "JSON input validation failure",
    );
}

#[test]
fn json_side_file_open_failure_preserves_non_utf8_path() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let output_path = raw_path(directory.path(), b"output.json");
    let prefix = directory.path().join(raw(b"missing-\xff")).join("stream");
    let prefix_arg = OsString::from_vec(
        [
            b"--json-stream-prefix=".as_slice(),
            prefix.as_os_str().as_bytes(),
        ]
        .concat(),
    );
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from("--json-output=2"),
        OsString::from("--json-stream-data=file"),
        prefix_arg,
        input.into_os_string(),
        output_path.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        prefix.as_os_str().as_bytes(),
        "JSON side-file open failure",
    );
}

#[test]
fn attachment_empty_report_preserves_non_utf8_input_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input-\xff.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from("--list-attachments"),
        input.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        input.as_os_str().as_bytes(),
        "empty attachment report",
    );
}

#[test]
fn attachment_verbose_report_preserves_non_utf8_source_path() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let payload = raw_path(directory.path(), b"payload-\xff.bin");
    let output_path = raw_path(directory.path(), b"attached.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");
    fs::write(&payload, b"attachment payload").expect("write payload");

    let output = run([
        OsString::from("--verbose"),
        OsString::from("--add-attachment"),
        payload.clone().into_os_string(),
        OsString::from("--"),
        input.into_os_string(),
        output_path.into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert_preserves_raw_bytes(
        &output,
        payload.as_os_str().as_bytes(),
        "attachment verbose report",
    );
}

#[test]
fn json_stream_prefix_preserves_non_utf8_side_file_path() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let json_output = raw_path(directory.path(), b"output.json");
    let prefix = raw_path(directory.path(), b"stream-\xff");
    let prefix_arg = OsString::from_vec(
        [
            b"--json-stream-prefix=".as_slice(),
            prefix.as_os_str().as_bytes(),
        ]
        .concat(),
    );
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from("--json-output=2"),
        OsString::from("--json-stream-data=file"),
        prefix_arg,
        input.into_os_string(),
        json_output.clone().into_os_string(),
    ]);

    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    let side_file = raw_path(directory.path(), b"stream-\xff-7");
    assert!(side_file.exists(), "raw JSON side file was not created");
    let json = fs::read(json_output).expect("read JSON output");
    assert!(contains_bytes(&json, b"stream-\xff-7"));
    assert!(!contains_bytes(&json, b"\xef\xbf\xbd"));
}

#[test]
fn split_pages_preserves_non_utf8_output_template_bytes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = raw_path(directory.path(), b"input.pdf");
    let template = raw_path(directory.path(), b"split-\xff.pdf");
    let percent_template = raw_path(directory.path(), b"percent-\xff-%d.pdf");
    fs::copy(ONE_PAGE, &input).expect("copy input");

    let output = run([
        OsString::from("--split-pages=1"),
        input.clone().into_os_string(),
        template.into_os_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert!(raw_path(directory.path(), b"split-\xff-1.pdf").exists());

    let output = run([
        OsString::from("--split-pages=1"),
        input.into_os_string(),
        percent_template.into_os_string(),
    ]);
    assert_eq!(output.status.code(), Some(0), "stderr={:?}", output.stderr);
    assert!(raw_path(directory.path(), b"percent-\xff-1.pdf").exists());
}
