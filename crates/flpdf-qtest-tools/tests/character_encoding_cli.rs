#![cfg(unix)]

use assert_cmd::Command;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

const PDFDOC_BIN: &str = "flpdf-test-pdf-doc-encoding";
const UNICODE_BIN: &str = "flpdf-test-pdf-unicode";

fn helper(name: &str) -> Command {
    Command::cargo_bin(name).unwrap_or_else(|error| panic!("{name} binary: {error}"))
}

fn helper_path(name: &str) -> PathBuf {
    PathBuf::from(helper(name).get_program())
}

fn write_input(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, bytes).expect("write authored input");
    path
}

#[test]
fn pdfdoc_helper_matches_qpdf_line_and_encoding_rules() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = write_input(
        directory.path(),
        "pdfdoc.in",
        b"plain\r\n\x80 bullet\rbare\nlast",
    );

    helper(PDFDOC_BIN)
        .arg(input)
        .assert()
        .code(0)
        .stdout(b"plain\n\xe2\x80\xa2 bullet\rbare\nlast\n".as_slice())
        .stderr("");
}

#[test]
fn unicode_helper_matches_pdfdoc_utf16_bom_and_malformed_input_rules() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = write_input(
        directory.path(),
        "unicode.in",
        b"ASCII\nEuro \xe2\x82\xac\n\xf0\x9f\xa5\x94\n\xc3\xbe\xc3\xbf\n\xfeafter",
    );
    let expected = concat!(
        "ASCII // <4153434949>\n",
        "Euro € // <4575726f20a0>\n",
        "🥔 // <feffd83edd54>\n",
        "þÿ // <feff00fe00ff>\n",
        "�after // <fefffffd00610066007400650072>\n",
    );

    helper(UNICODE_BIN)
        .arg(input)
        .assert()
        .code(0)
        .stdout(expected)
        .stderr("");
}

#[test]
fn empty_blank_and_terminal_lf_inputs_remain_distinct() {
    let directory = tempfile::tempdir().expect("temporary directory");
    for (name, input, expected) in [
        ("empty.in", b"".as_slice(), b"".as_slice()),
        ("blank.in", b"\n".as_slice(), b"\n".as_slice()),
        ("terminal.in", b"x\n".as_slice(), b"x\n".as_slice()),
        ("bare-cr.in", b"x\r".as_slice(), b"x\r\n".as_slice()),
    ] {
        let input = write_input(directory.path(), name, input);
        helper(PDFDOC_BIN)
            .arg(input)
            .assert()
            .code(0)
            .stdout(expected)
            .stderr("");
    }
}

#[test]
fn both_helpers_require_exactly_one_input() {
    helper(PDFDOC_BIN)
        .assert()
        .code(2)
        .stdout("")
        .stderr(format!("Usage: {PDFDOC_BIN} infile\n"));
    helper(UNICODE_BIN)
        .args(["one", "two"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(format!("Usage: {UNICODE_BIN} infile\n"));
}

#[test]
fn usage_preserves_qpdfs_forward_slash_only_argv0_rule() {
    let argv0 = OsString::from_vec(b"probe/path\\helper.exe".to_vec());
    let output = StdCommand::new(helper_path(PDFDOC_BIN))
        .arg0(argv0)
        .output()
        .expect("run PDFDoc helper");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"Usage: path\\helper.exe infile\n");
}

#[test]
fn missing_input_matches_qpdf_system_error_and_aborts() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing.in");
    let output = StdCommand::new(helper_path(PDFDOC_BIN))
        .arg(&missing)
        .output()
        .expect("run PDFDoc helper");
    let expected = format!(
        "terminate called after throwing an instance of 'QPDFSystemError'\n  \
         what():  open {}: No such file or directory\n",
        missing.display()
    );

    assert_eq!(output.status.signal(), Some(libc::SIGABRT));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, expected.as_bytes());
}

#[test]
fn directory_read_matches_qpdf_runtime_error_and_aborts() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = StdCommand::new(helper_path(UNICODE_BIN))
        .arg(directory.path())
        .output()
        .expect("run Unicode helper");

    assert_eq!(output.status.signal(), Some(libc::SIGABRT));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"terminate called after throwing an instance of 'std::runtime_error'\n  \
          what():  failure reading character from file\n"
    );
}

#[test]
fn missing_non_utf8_path_is_reported_as_raw_bytes_before_abort() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing_name = OsString::from_vec(b"missing-\xff.in".to_vec());
    let missing = directory.path().join(&missing_name);
    let output = StdCommand::new(helper_path(UNICODE_BIN))
        .arg(&missing)
        .output()
        .expect("run Unicode helper");
    let mut expected =
        b"terminate called after throwing an instance of 'QPDFSystemError'\n  what():  open "
            .to_vec();
    expected.extend_from_slice(missing.as_os_str().as_bytes());
    expected.extend_from_slice(b": No such file or directory\n");

    assert_eq!(output.status.signal(), Some(libc::SIGABRT));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, expected);
}
