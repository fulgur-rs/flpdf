use std::{
    borrow::Cow,
    ffi::{CStr, OsStr, OsString},
    io::{self, Write},
};

#[cfg(unix)]
use std::ffi::CString;

use flpdf::{Diagnostic, Error, Pdf, PdfOpenOptions};

use crate::common::test_driver_program_name_bytes;

pub(crate) mod handle;
pub(crate) mod test_0_1;

pub fn run(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    let whoami = args
        .first()
        .map(OsString::as_os_str)
        .map(os_str_diagnostic_bytes)
        .unwrap_or_else(|| Cow::Borrowed(b"flpdf-test-driver"));
    let whoami = test_driver_program_name_bytes(&whoami);
    if args.len() < 3 || args.len() > 4 {
        let mut usage = b"Usage: ".to_vec();
        usage.extend_from_slice(whoami);
        usage.extend_from_slice(b" n filename1 [arg2]");
        return write_error_bytes(stdout, stderr, &usage);
    }

    let test_number = os_str_diagnostic_bytes(args[1].as_os_str());
    let n = match parse_test_number(&test_number) {
        Ok(n) => n,
        Err(error) => return write_error_bytes(stdout, stderr, &error),
    };
    let filename = args[2].as_os_str();
    let filename_diagnostic = os_str_diagnostic_bytes(filename);
    let bytes = match std::fs::read(filename) {
        Ok(bytes) => bytes,
        Err(error) => {
            let crt_message = crt_open_error_message(filename);
            return write_error_bytes(
                stdout,
                stderr,
                &open_error_bytes(&filename_diagnostic, crt_message.as_deref(), &error),
            );
        }
    };
    let options = PdfOpenOptions {
        repair: n != 0,
        // The compatibility driver owns byte-exact warning formatting and
        // routes it through the caller-supplied stdout/stderr writers below.
        suppress_warnings: true,
        ..PdfOpenOptions::default()
    };
    let mut pdf = match Pdf::open_mem_owned_with_options(bytes, options) {
        Ok(pdf) => pdf,
        Err(error) => {
            return write_open_failure(n, &filename_diagnostic, &error, stdout, stderr);
        }
    };

    let mut diagnostics_written = 0;
    if emit_new_diagnostics(
        &pdf,
        &mut diagnostics_written,
        &filename_diagnostic,
        stdout,
        stderr,
    )
    .is_err()
    {
        return 2;
    }

    if n != 0 && n != 1 {
        return write_error(stdout, stderr, &format!("invalid test {n}"));
    }
    if let Err(error) = test_0_1::run_test_0_1(
        &mut pdf,
        &filename_diagnostic,
        stdout,
        stderr,
        &mut diagnostics_written,
    ) {
        return write_error(stdout, stderr, &error.to_string());
    }
    if writeln!(stdout, "test {n} done").is_err() {
        return 2;
    }
    0
}

fn open_pdf_error_bytes(n: i32, filename: &[u8], error: &Error) -> Vec<u8> {
    let suffix: Option<Cow<str>> = match error {
        Error::Parse { message, .. } if n == 0 && message == "xref not found" => {
            Some(Cow::Borrowed(": can't find startxref"))
        }
        // Both of `reconstruct_xref`'s terminal errors throw via the same
        // `damagedPDF("", 0, message)` (`QPDF.cc:601-604,614`), which
        // `QPDFExc::createWhat` (`QPDFExc.cc:18-51`) formats identically as
        // `"<filename>: <message>"`; only flpdf's own "parse error at byte
        // N: " `Display` prefix -- which qpdf's real test-driver never
        // prints for either -- needs stripping here.
        Error::Parse { message, .. }
            if n != 0
                && (message == "unable to find trailer dictionary while recovering damaged file"
                    || message
                        == "error decoding candidate xref stream while recovering damaged file") =>
        {
            Some(Cow::Owned(format!(": {message}")))
        }
        _ => None,
    };
    if let Some(suffix) = suffix {
        let mut output = filename.to_vec();
        output.extend_from_slice(suffix.as_bytes());
        output
    } else {
        error.to_string().into_bytes()
    }
}

fn write_open_failure(
    n: i32,
    filename: &[u8],
    error: &Error,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let source = if let Some((source, diagnostics)) = error.open_failure() {
        for diagnostic in diagnostics.entries() {
            if write_warning(filename, diagnostic, stdout, stderr).is_err() {
                return 2;
            }
        }
        source
    } else {
        error
    };
    write_error_bytes(stdout, stderr, &open_pdf_error_bytes(n, filename, source))
}

pub(crate) fn open_error_bytes(
    filename: &[u8],
    crt_message: Option<&[u8]>,
    fallback: &io::Error,
) -> Vec<u8> {
    let message = crt_message
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback.to_string().into_bytes());
    let mut output = b"open ".to_vec();
    output.extend_from_slice(filename);
    output.extend_from_slice(b": ");
    output.extend_from_slice(&message);
    output
}

fn strerror_bytes(error_code: libc::c_int) -> Option<Vec<u8>> {
    let message = unsafe { libc::strerror(error_code) };
    (!message.is_null()).then(|| unsafe { CStr::from_ptr(message) }.to_bytes().to_vec())
}

#[cfg(unix)]
pub(crate) fn crt_open_error_message(filename: &OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;

    let filename = CString::new(filename.as_bytes()).ok()?;
    let mode = CString::new("rb").expect("literal contains no NUL");
    let file = unsafe { libc::fopen(filename.as_ptr(), mode.as_ptr()) };
    if !file.is_null() {
        // Rust's initial read failed but this second, diagnostic CRT open won a race.
        // It provides no matching errno, so preserve the original Rust error as fallback.
        let _ = unsafe { libc::fclose(file) };
        return None;
    }
    let error_code = io::Error::last_os_error().raw_os_error()?;
    strerror_bytes(error_code)
}

#[cfg(windows)]
unsafe extern "C" {
    fn _wfopen_s(
        file: *mut *mut libc::FILE,
        filename: *const libc::wchar_t,
        mode: *const libc::wchar_t,
    ) -> libc::c_int;
}

#[cfg(all(unix, test))]
fn has_interior_nul(filename: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;

    filename.as_bytes().contains(&0)
}

#[cfg(windows)]
fn has_interior_nul(filename: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    filename.encode_wide().any(|unit| unit == 0)
}

#[cfg(windows)]
pub(crate) fn crt_open_error_message(filename: &OsStr) -> Option<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;

    if has_interior_nul(filename) {
        // `_wfopen_s` would stop at the NUL and probe a different path. With no
        // CRT evidence for Rust's failed path, preserve the original fallback.
        return None;
    }
    let filename: Vec<libc::wchar_t> = filename.encode_wide().chain(std::iter::once(0)).collect();
    let mode = [b'r' as libc::wchar_t, b'b' as libc::wchar_t, 0];
    let mut file = std::ptr::null_mut();
    let error_code = unsafe { _wfopen_s(&mut file, filename.as_ptr(), mode.as_ptr()) };
    if error_code == 0 {
        // Rust's initial read failed but this second, diagnostic CRT open won a race.
        // It provides no matching errno, so preserve the original Rust error as fallback.
        if !file.is_null() {
            let _ = unsafe { libc::fclose(file) };
        }
        return None;
    }
    strerror_bytes(error_code)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn crt_open_error_message(_filename: &OsStr) -> Option<Vec<u8>> {
    None
}

#[cfg(unix)]
pub(crate) fn os_str_diagnostic_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;

    Cow::Borrowed(value.as_bytes())
}

#[cfg(not(unix))]
pub(crate) fn os_str_diagnostic_bytes(value: &OsStr) -> Cow<'_, [u8]> {
    // This fallback is lossy only for unpaired wide values. Valid-Unicode Windows
    // diagnostics remain byte-identical to their prior UTF-8 output.
    Cow::Owned(value.to_string_lossy().into_owned().into_bytes())
}

fn decimal_error(prefix: &[u8], input: &[u8], suffix: &[u8]) -> Vec<u8> {
    let mut message = prefix.to_vec();
    message.extend_from_slice(input);
    message.extend_from_slice(suffix);
    message
}

fn parse_test_number(input: &[u8]) -> Result<i32, Vec<u8>> {
    let bytes = input;
    let mut index = 0;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }

    let negative = match bytes.get(index) {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };

    let mut value = 0_u64;
    let mut consumed_digit = false;
    while let Some(digit) = bytes.get(index).and_then(|byte| byte.checked_sub(b'0')) {
        if digit > 9 {
            break;
        }
        consumed_digit = true;
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit)))
            .ok_or_else(|| {
                decimal_error(
                    b"overflow/underflow converting ",
                    input,
                    b" to 64-bit integer",
                )
            })?;
        index += 1;
    }

    if !consumed_digit {
        return Ok(0);
    }

    let i64_value = if negative {
        const I64_MIN_MAGNITUDE: u64 = 9_223_372_036_854_775_808;
        if value > I64_MIN_MAGNITUDE {
            return Err(decimal_error(
                b"overflow/underflow converting ",
                input,
                b" to 64-bit integer",
            ));
        }
        if value == I64_MIN_MAGNITUDE {
            i64::MIN
        } else {
            -(value as i64)
        }
    } else {
        if value > i64::MAX as u64 {
            return Err(decimal_error(
                b"overflow/underflow converting ",
                input,
                b" to 64-bit integer",
            ));
        }
        value as i64
    };

    i32::try_from(i64_value).map_err(|_| {
        format!(
            "integer out of range converting {i64_value} from a 8-byte signed type to a 4-byte signed type"
        )
        .into_bytes()
    })
}

pub(crate) fn emit_new_diagnostics<R: io::Read + io::Seek>(
    pdf: &Pdf<R>,
    diagnostics_written: &mut usize,
    filename: &[u8],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let diagnostics = pdf.repair_diagnostics();
    let entries = diagnostics.entries();
    for diagnostic in &entries[*diagnostics_written..] {
        write_warning(filename, diagnostic, stdout, stderr)?;
    }
    *diagnostics_written = entries.len();
    Ok(())
}

pub(crate) fn write_warning(
    filename: &[u8],
    diagnostic: &Diagnostic,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let message = diagnostic.message.as_str();
    let offset = diagnostic.offset;
    let mut line = b"WARNING: ".to_vec();
    line.extend_from_slice(filename);
    if message.starts_with('(') {
        line.push(b' ');
    } else if let Some(offset) = offset {
        line.extend_from_slice(format!(" (offset {offset}): ").as_bytes());
    } else {
        line.extend_from_slice(b": ");
    }
    line.extend_from_slice(message.as_bytes());
    write_stderr_bytes(stdout, stderr, &line)
}

fn write_error(stdout: &mut dyn Write, stderr: &mut dyn Write, message: &str) -> u8 {
    write_error_bytes(stdout, stderr, message.as_bytes())
}

fn write_error_bytes(stdout: &mut dyn Write, stderr: &mut dyn Write, message: &[u8]) -> u8 {
    let _ = write_stderr_bytes(stdout, stderr, message);
    2
}

fn write_stderr_bytes(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    message: &[u8],
) -> io::Result<()> {
    stdout.flush()?;
    stderr.write_all(message)?;
    stderr.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::{
        crt_open_error_message, has_interior_nul, open_error_bytes, open_pdf_error_bytes, run,
        write_error_bytes,
    };
    use std::{
        ffi::{OsStr, OsString},
        io::{self, Write},
    };

    #[cfg(unix)]
    #[test]
    fn usage_preserves_non_utf8_backslash_and_exe_suffix() {
        use std::os::unix::ffi::OsStringExt;

        let args = vec![OsString::from_vec(b"/tmp/test-\xff\\driver.exe".to_vec())];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"Usage: test-\xff\\driver.exe n filename1 [arg2]\n");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_test_number_without_decimal_prefix_dispatches_zero() {
        use std::os::unix::ffi::OsStringExt;

        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from_vec(vec![0xff]),
            OsString::from(fixture("direct_null")),
        ];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(run(&args, &mut stdout, &mut stderr), 0);
        assert!(stdout.ends_with(b"test 0 done\n"));
        assert!(stderr.is_empty());
    }

    fn fixture(name: &str) -> String {
        format!(
            "{}/../../tests/fixtures/test_driver/{name}.pdf",
            env!("CARGO_MANIFEST_DIR")
        )
    }

    #[test]
    fn open_error_bytes_preserve_non_utf8_crt_message_bytes() {
        let fallback = io::Error::other("fallback must not be used");
        assert_eq!(
            open_error_bytes(b"input.pdf", Some(&[0xff, b'!']), &fallback),
            b"open input.pdf: \xff!"
        );
    }

    #[test]
    fn interior_nul_guard_rejects_a_path_that_would_be_truncated_by_the_crt() {
        assert!(has_interior_nul(OsStr::new("before\0after")));
        assert!(!has_interior_nul(OsStr::new("ordinary.pdf")));
    }

    #[test]
    fn open_error_bytes_fall_back_only_without_a_crt_message() {
        let fallback = io::Error::other("fallback message");
        assert_eq!(
            open_error_bytes(b"input.pdf", None, &fallback),
            b"open input.pdf: fallback message"
        );
    }

    #[test]
    fn ordinary_pdf_open_error_uses_the_error_display() {
        let error = flpdf::Error::System("ordinary open failure".to_string());

        assert_eq!(
            open_pdf_error_bytes(1, b"input.pdf", &error),
            b"ordinary open failure"
        );
    }

    #[test]
    fn no_trailer_candidate_error_gets_the_qpdf_filename_prefix() {
        let error = flpdf::Error::parse(
            0,
            "unable to find trailer dictionary while recovering damaged file",
        );

        assert_eq!(
            open_pdf_error_bytes(1, b"input.pdf", &error),
            b"input.pdf: unable to find trailer dictionary while recovering damaged file"
        );
    }

    #[test]
    fn candidate_decode_failure_error_gets_the_qpdf_filename_prefix() {
        // `QPDFExc::createWhat` (`QPDFExc.cc:18-51`) wraps every
        // `damagedPDF("", 0, message)` throw -- both `reconstruct_xref`
        // terminal errors use it (`QPDF.cc:601-604,614`) -- as
        // `"<filename>: <message>"`. Only the "no candidate at all" branch
        // had this treatment; the newer "candidate found but undecodable"
        // message must get the identical prefix, not flpdf's own
        // "parse error at byte N: " `Display` wording.
        let error = flpdf::Error::parse(
            0,
            "error decoding candidate xref stream while recovering damaged file",
        );

        assert_eq!(
            open_pdf_error_bytes(1, b"input.pdf", &error),
            b"input.pdf: error decoding candidate xref stream while recovering damaged file"
        );
    }

    #[test]
    fn byte_error_writer_emits_raw_message_bytes_and_newline() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            write_error_bytes(&mut stdout, &mut stderr, &[0xff, b'!']),
            2
        );
        assert_eq!(stderr, b"\xff!\n");
    }

    #[cfg(unix)]
    #[test]
    fn unix_crt_open_failure_supplies_raw_strerror_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing.pdf");
        let message = crt_open_error_message(missing.as_os_str())
            .expect("fopen failure must supply strerror bytes");
        assert!(!message.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unix_crt_open_success_is_not_misreported_as_an_error() {
        assert!(crt_open_error_message(OsStr::new(&fixture("direct_null"))).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_crt_open_failure_supplies_raw_strerror_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing.pdf");
        let message = crt_open_error_message(missing.as_os_str())
            .expect("_wfopen_s failure must supply strerror bytes");
        assert!(!message.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_crt_probe_skips_an_interior_nul_path() {
        assert!(has_interior_nul(OsStr::new("before\0after")));
        assert!(crt_open_error_message(OsStr::new("before\0after")).is_none());
    }

    struct FlushFailure;

    impl Write for FlushFailure {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    struct WriteFailure;

    impl Write for WriteFailure {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FirstWriteFailure {
        attempted: Vec<u8>,
        writes: usize,
    }

    impl Write for FirstWriteFailure {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.attempted.extend_from_slice(buf);
            Err(io::Error::other("warning write failed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct FooterFailure {
        bytes: Vec<u8>,
    }

    impl Write for FooterFailure {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf.windows(b"test ".len()).any(|window| window == b"test ") {
                return Err(io::Error::other("footer failed"));
            }
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn repair_warning_flush_failure_exits_two() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("repairable_input")),
        ];
        let mut stdout = FlushFailure;
        let mut stderr = Vec::new();
        assert_eq!(stdout.write(b"probe").expect("probe write"), 5);
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stderr.is_empty());
    }

    #[test]
    fn failed_open_warning_write_failure_skips_terminal_error() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("open_repair_failure")),
        ];
        let mut stdout = Vec::new();
        let mut stderr = FirstWriteFailure::default();

        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert_eq!(stderr.writes, 1);
        assert!(stderr.attempted.starts_with(b"WARNING:"));
        assert!(!stderr
            .attempted
            .windows(b"unable to recover".len())
            .any(|window| window == b"unable to recover"));
        stderr.flush().expect("flush failure writer");
    }

    #[test]
    fn test_body_write_failure_is_reported_and_exits_two() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("direct_null")),
        ];
        let mut stdout = WriteFailure;
        let mut stderr = Vec::new();
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert_eq!(stderr, b"I/O error: write failed\n");
    }

    #[test]
    fn footer_write_failure_exits_two() {
        let args = vec![
            OsString::from("flpdf-test-driver"),
            OsString::from("1"),
            OsString::from(fixture("direct_null")),
        ];
        let mut stdout = FooterFailure::default();
        let mut stderr = Vec::new();
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stderr.is_empty());
        assert!(stdout.bytes.ends_with(b"unparseResolved: null\n"));
        stdout.flush().expect("flush footer writer");
    }
}
