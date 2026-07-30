//! Ports of qpdf's PDFDocEncoding and Unicode-string test helpers.

use std::borrow::Cow;
use std::ffi::{CStr, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use crate::common::test_driver_program_name_bytes;

/// Terminal action requested by one helper runner.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Exit normally with this process status.
    Exit(u8),
    /// Write these bytes to stderr and terminate with `SIGABRT`.
    Abort(Vec<u8>),
}

enum Mode {
    PdfDoc,
    Unicode,
}

/// Run the qpdf `test_pdf_doc_encoding` contract.
pub fn run_pdf_doc_encoding(
    args: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> RunOutcome {
    run(args, stdout, stderr, Mode::PdfDoc)
}

/// Run the qpdf `test_pdf_unicode` contract.
pub fn run_pdf_unicode(
    args: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> RunOutcome {
    run(args, stdout, stderr, Mode::Unicode)
}

/// Apply a runner outcome to the current process.
pub fn finish(outcome: RunOutcome, stderr: &mut dyn Write) -> ExitCode {
    match outcome {
        RunOutcome::Exit(status) => ExitCode::from(status),
        // cov:ignore-start: subprocess integration verifies exact stderr and SIGABRT; abort cannot flush an in-process coverage profile
        RunOutcome::Abort(message) => {
            let _ = stderr.write_all(&message);
            let _ = stderr.flush();
            std::process::abort()
        } // cov:ignore-end
    }
}

fn run(
    args: &[OsString],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    mode: Mode,
) -> RunOutcome {
    let whoami = args
        .first()
        .map(OsString::as_os_str)
        .map(os_str_bytes)
        .unwrap_or_else(|| {
            Cow::Borrowed(match mode {
                Mode::PdfDoc => b"flpdf-test-pdf-doc-encoding",
                Mode::Unicode => b"flpdf-test-pdf-unicode",
            })
        });
    let whoami = test_driver_program_name_bytes(&whoami);
    if args.len() != 2 {
        let mut usage = b"Usage: ".to_vec();
        usage.extend_from_slice(whoami);
        usage.extend_from_slice(b" infile\n");
        let _ = stderr.write_all(&usage);
        return RunOutcome::Exit(2);
    }

    let filename = args[1].as_os_str();
    let mut input = Vec::new();
    let mut file = match File::open(filename) {
        Ok(file) => file,
        Err(error) => return RunOutcome::Abort(open_exception(filename, &error)),
    };
    if file.read_to_end(&mut input).is_err() {
        return RunOutcome::Abort(uncaught_exception(
            b"std::runtime_error",
            b"failure reading character from file",
        ));
    }

    for line in qpdf_lines(&input) {
        let write_result = match mode {
            Mode::PdfDoc => {
                let value = flpdf::qtest_string::utf8_value(line);
                stdout.write_all(&value)
            }
            Mode::Unicode => {
                let stored = flpdf::qtest_string::new_unicode_string(line);
                let value = flpdf::qtest_string::utf8_value(&stored);
                let binary = flpdf::qtest_string::unparse_binary(&stored);
                stdout
                    .write_all(&value)
                    .and_then(|()| stdout.write_all(b" // "))
                    .and_then(|()| stdout.write_all(&binary))
            }
        };
        if write_result.and_then(|()| stdout.write_all(b"\n")).is_err() {
            return RunOutcome::Exit(2);
        }
    }
    RunOutcome::Exit(0)
}

fn qpdf_lines(input: &[u8]) -> impl Iterator<Item = &[u8]> {
    input.split_inclusive(|byte| *byte == b'\n').map(|line| {
        if let Some(line) = line.strip_suffix(b"\n") {
            line.strip_suffix(b"\r").unwrap_or(line)
        } else {
            line
        }
    })
}

fn open_exception(filename: &OsStr, error: &io::Error) -> Vec<u8> {
    let mut message = b"open ".to_vec();
    message.extend_from_slice(&os_str_bytes(filename));
    message.extend_from_slice(b": ");
    message.extend_from_slice(&native_error_message(error));
    uncaught_exception(b"QPDFSystemError", &message)
}

fn uncaught_exception(class: &[u8], message: &[u8]) -> Vec<u8> {
    let mut output = b"terminate called after throwing an instance of '".to_vec();
    output.extend_from_slice(class);
    output.extend_from_slice(b"'\n  what():  ");
    output.extend_from_slice(message);
    output.push(b'\n');
    output
}

fn native_error_message(error: &io::Error) -> Vec<u8> {
    let Some(error_code) = error.raw_os_error() else {
        return error.to_string().into_bytes();
    };
    let message = unsafe { libc::strerror(error_code) };
    if message.is_null() {
        error.to_string().into_bytes() // cov:ignore: glibc returns an "Unknown error" string even for unrecognized error numbers
    } else {
        unsafe { CStr::from_ptr(message) }.to_bytes().to_vec()
    }
}

#[cfg(unix)]
fn os_str_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;

    Cow::Borrowed(value.as_bytes())
}

#[cfg(not(unix))]
fn os_str_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    Cow::Owned(value.to_string_lossy().into_owned().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    struct FailedWriter;

    impl Write for FailedWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("authored write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn args(program: &str, input: &Path) -> Vec<OsString> {
        vec![OsString::from(program), input.as_os_str().to_owned()]
    }

    #[test]
    fn missing_argv_uses_each_helpers_default_program_name() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_pdf_doc_encoding(&[], &mut stdout, &mut stderr),
            RunOutcome::Exit(2)
        );
        assert_eq!(stderr, b"Usage: flpdf-test-pdf-doc-encoding infile\n");

        stderr.clear();
        assert_eq!(
            run_pdf_unicode(&[], &mut stdout, &mut stderr),
            RunOutcome::Exit(2)
        );
        assert_eq!(stderr, b"Usage: flpdf-test-pdf-unicode infile\n");
    }

    #[test]
    fn missing_input_returns_the_qpdf_system_exception_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing.in");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let outcome = run_pdf_doc_encoding(
            &args("test_pdf_doc_encoding", &missing),
            &mut stdout,
            &mut stderr,
        );

        let RunOutcome::Abort(message) = outcome else {
            panic!("missing input must request abort");
        };
        assert!(message.starts_with(
            b"terminate called after throwing an instance of 'QPDFSystemError'\n  what():  open "
        ));
        assert!(message.ends_with(b": No such file or directory\n"));
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn directory_input_returns_the_qpdf_runtime_exception_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            run_pdf_unicode(
                &args("test_pdf_unicode", directory.path()),
                &mut stdout,
                &mut stderr,
            ),
            RunOutcome::Abort(
                b"terminate called after throwing an instance of 'std::runtime_error'\n  \
                  what():  failure reading character from file\n"
                    .to_vec()
            )
        );
    }

    #[test]
    fn output_failure_returns_status_two() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let input = directory.path().join("input");
        std::fs::write(&input, b"line\n").expect("write input");
        let mut stdout = FailedWriter;
        let mut stderr = Vec::new();

        assert_eq!(
            run_pdf_doc_encoding(
                &args("test_pdf_doc_encoding", &input),
                &mut stdout,
                &mut stderr,
            ),
            RunOutcome::Exit(2)
        );
    }

    #[test]
    fn native_error_without_errno_uses_display_text() {
        let error = io::Error::other("authored native error");
        assert_eq!(native_error_message(&error), b"authored native error");
    }
}
