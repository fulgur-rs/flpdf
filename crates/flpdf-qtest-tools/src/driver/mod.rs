use std::io::{self, Write};

use flpdf::{Pdf, PdfOpenOptions};

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

    let n = match args[1].parse::<i32>() {
        Ok(n) => n,
        Err(error) => return write_error(stdout, stderr, &error.to_string()),
    };
    let filename = &args[2];
    let bytes = match std::fs::read(filename) {
        Ok(bytes) => bytes,
        Err(error) => return write_error(stdout, stderr, &error.to_string()),
    };
    let options = PdfOpenOptions {
        repair: true,
        ..PdfOpenOptions::default()
    };
    let mut pdf = match Pdf::open_mem_with_options(&bytes, options) {
        Ok(pdf) => pdf,
        Err(error) => return write_error(stdout, stderr, &error.to_string()),
    };

    for diagnostic in pdf.repair_diagnostics().entries() {
        let line = match diagnostic.offset {
            Some(offset) => format!(
                "WARNING: {filename} (offset {offset}): {}",
                diagnostic.message
            ),
            None => format!("WARNING: {filename}: {}", diagnostic.message),
        };
        if write_stderr_line(stdout, stderr, &line).is_err() {
            return 2;
        }
    }

    if n != 1 {
        return write_error(stdout, stderr, &format!("invalid test {n}"));
    }
    if let Err(error) = test_0_1::run_test_0_1(&mut pdf, stdout) {
        return write_error(stdout, stderr, &error.to_string());
    }
    if writeln!(stdout, "test {n} done").is_err() {
        return 2;
    }
    0
}

fn write_error(stdout: &mut dyn Write, stderr: &mut dyn Write, message: &str) -> u8 {
    let _ = write_stderr_line(stdout, stderr, message);
    2
}

fn write_stderr_line(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    message: &str,
) -> io::Result<()> {
    stdout.flush()?;
    writeln!(stderr, "{message}")
}
