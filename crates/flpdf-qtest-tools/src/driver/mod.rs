use std::{
    ffi::CStr,
    io::{self, Write},
};

#[cfg(unix)]
use std::ffi::CString;

use flpdf::{Diagnostic, Error, Pdf, PdfOpenOptions};

use crate::common::program_name;

pub(crate) mod handle;
pub(crate) mod test_0_1;

pub fn run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> u8 {
    let whoami = program_name(
        args.first()
            .map(String::as_str)
            .unwrap_or("flpdf-test-driver"),
    );
    if args.len() < 3 || args.len() > 4 {
        return write_error(
            stdout,
            stderr,
            &format!("Usage: {whoami} n filename1 [arg2]"),
        );
    }

    let n = match parse_test_number(&args[1]) {
        Ok(n) => n,
        Err(error) => return write_error(stdout, stderr, &error),
    };
    let filename = &args[2];
    let bytes = match std::fs::read(filename) {
        Ok(bytes) => bytes,
        Err(error) => {
            let crt_message = crt_open_error_message(filename);
            return write_error_bytes(
                stdout,
                stderr,
                &open_error_bytes(filename, crt_message.as_deref(), &error),
            );
        }
    };
    let options = PdfOpenOptions {
        repair: n != 0,
        ..PdfOpenOptions::default()
    };
    let mut pdf = match Pdf::open_mem_with_options(&bytes, options) {
        Ok(pdf) => pdf,
        Err(error) => return write_error(stdout, stderr, &open_pdf_error(n, filename, &error)),
    };

    let mut diagnostics_written = 0;
    if emit_new_diagnostics(&pdf, &mut diagnostics_written, filename, stdout, stderr).is_err() {
        return 2;
    }

    if n != 0 && n != 1 {
        return write_error(stdout, stderr, &format!("invalid test {n}"));
    }
    if let Err(error) =
        test_0_1::run_test_0_1(&mut pdf, filename, stdout, stderr, &mut diagnostics_written)
    {
        return write_error(stdout, stderr, &error.to_string());
    }
    if writeln!(stdout, "test {n} done").is_err() {
        return 2;
    }
    0
}

fn open_pdf_error(n: i32, filename: &str, error: &Error) -> String {
    match error {
        Error::Parse { message, .. } if n == 0 && message == "xref not found" => {
            format!("{filename}: can't find startxref")
        }
        _ => error.to_string(),
    }
}

fn open_error_bytes(filename: &str, crt_message: Option<&[u8]>, fallback: &io::Error) -> Vec<u8> {
    let message = crt_message
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback.to_string().into_bytes());
    let mut output = b"open ".to_vec();
    output.extend_from_slice(filename.as_bytes());
    output.extend_from_slice(b": ");
    output.extend_from_slice(&message);
    output
}

fn strerror_bytes(error_code: libc::c_int) -> Option<Vec<u8>> {
    let message = unsafe { libc::strerror(error_code) };
    (!message.is_null()).then(|| unsafe { CStr::from_ptr(message) }.to_bytes().to_vec())
}

#[cfg(unix)]
fn crt_open_error_message(filename: &str) -> Option<Vec<u8>> {
    let filename = CString::new(filename).ok()?;
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

#[cfg(windows)]
fn crt_open_error_message(filename: &str) -> Option<Vec<u8>> {
    let filename: Vec<libc::wchar_t> = filename.encode_utf16().chain(std::iter::once(0)).collect();
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
fn crt_open_error_message(_filename: &str) -> Option<Vec<u8>> {
    None
}

fn parse_test_number(input: &str) -> Result<i32, String> {
    let bytes = input.as_bytes();
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
            .ok_or_else(|| format!("overflow/underflow converting {input} to 64-bit integer"))?;
        index += 1;
    }

    if !consumed_digit {
        return Ok(0);
    }

    let i64_value = if negative {
        const I64_MIN_MAGNITUDE: u64 = 9_223_372_036_854_775_808;
        if value > I64_MIN_MAGNITUDE {
            return Err(format!(
                "overflow/underflow converting {input} to 64-bit integer"
            ));
        }
        if value == I64_MIN_MAGNITUDE {
            i64::MIN
        } else {
            -(value as i64)
        }
    } else {
        if value > i64::MAX as u64 {
            return Err(format!(
                "overflow/underflow converting {input} to 64-bit integer"
            ));
        }
        value as i64
    };

    i32::try_from(i64_value).map_err(|_| {
        format!(
            "integer out of range converting {i64_value} from a 8-byte signed type to a 4-byte signed type"
        )
    })
}

fn emit_new_diagnostics<R: io::Read + io::Seek>(
    pdf: &Pdf<R>,
    diagnostics_written: &mut usize,
    filename: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let entries = pdf.repair_diagnostics().entries();
    for diagnostic in &entries[*diagnostics_written..] {
        write_warning(filename, diagnostic, stdout, stderr)?;
    }
    *diagnostics_written = entries.len();
    Ok(())
}

fn write_warning(
    filename: &str,
    diagnostic: &Diagnostic,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let (message, offset) =
        if diagnostic.message == "xref not found" && diagnostic.offset == Some(0) {
            ("can't find startxref", None)
        } else {
            (diagnostic.message.as_str(), diagnostic.offset)
        };
    let line = if message.starts_with('(') {
        format!("WARNING: {filename} {message}")
    } else if let Some(offset) = offset {
        format!("WARNING: {filename} (offset {offset}): {message}")
    } else {
        format!("WARNING: {filename}: {message}")
    };
    write_stderr_line(stdout, stderr, &line)
}

fn write_error(stdout: &mut dyn Write, stderr: &mut dyn Write, message: &str) -> u8 {
    write_error_bytes(stdout, stderr, message.as_bytes())
}

fn write_error_bytes(stdout: &mut dyn Write, stderr: &mut dyn Write, message: &[u8]) -> u8 {
    let _ = write_stderr_bytes(stdout, stderr, message);
    2
}

fn write_stderr_line(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    message: &str,
) -> io::Result<()> {
    write_stderr_bytes(stdout, stderr, message.as_bytes())
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
    use super::{crt_open_error_message, open_error_bytes, run, write_error_bytes};
    use std::io::{self, Write};

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
            open_error_bytes("input.pdf", Some(&[0xff, b'!']), &fallback),
            b"open input.pdf: \xff!"
        );
    }

    #[test]
    fn open_error_bytes_fall_back_only_without_a_crt_message() {
        let fallback = io::Error::other("fallback message");
        assert_eq!(
            open_error_bytes("input.pdf", None, &fallback),
            b"open input.pdf: fallback message"
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
        let message = crt_open_error_message(missing.to_str().expect("utf-8 temp path"))
            .expect("fopen failure must supply strerror bytes");
        assert!(!message.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn unix_crt_open_success_is_not_misreported_as_an_error() {
        assert!(crt_open_error_message(&fixture("direct_null")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_crt_open_failure_supplies_raw_strerror_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let missing = directory.path().join("missing.pdf");
        let message = crt_open_error_message(missing.to_str().expect("utf-8 temp path"))
            .expect("_wfopen_s failure must supply strerror bytes");
        assert!(!message.is_empty());
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
            "flpdf-test-driver".to_string(),
            "1".to_string(),
            fixture("repairable_input"),
        ];
        let mut stdout = FlushFailure;
        let mut stderr = Vec::new();
        assert_eq!(stdout.write(b"probe").expect("probe write"), 5);
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_body_write_failure_is_reported_and_exits_two() {
        let args = vec![
            "flpdf-test-driver".to_string(),
            "1".to_string(),
            fixture("direct_null"),
        ];
        let mut stdout = WriteFailure;
        let mut stderr = Vec::new();
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert_eq!(stderr, b"I/O error: write failed\n");
    }

    #[test]
    fn footer_write_failure_exits_two() {
        let args = vec![
            "flpdf-test-driver".to_string(),
            "1".to_string(),
            fixture("direct_null"),
        ];
        let mut stdout = FooterFailure::default();
        let mut stderr = Vec::new();
        assert_eq!(run(&args, &mut stdout, &mut stderr), 2);
        assert!(stderr.is_empty());
        assert!(stdout.bytes.ends_with(b"unparseResolved: null\n"));
        stdout.flush().expect("flush footer writer");
    }
}
