//! Portable Rust translation of the qpdf C test helper's deterministic-ID case.
//!
//! qpdf's `qpdf-ctest.c:test19` (`qpdf-ctest.c:435-442`) intentionally tests
//! the C API lifecycle rather than a C symbol. Its observable sequence is:
//! read the input, initialize a writer for the output, enable deterministic
//! IDs, write, report errors, and print `C test 19 done`. Keep this adapter at
//! the qtest-tools process boundary; the PDF read/write responsibilities stay
//! in the canonical `flpdf::Pdf` and `flpdf::PdfWriter` APIs.

use flpdf::{Pdf, PdfOpenOptions, PdfWriter, Result};
use std::env;
use std::fs::File;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    match run(&args) {
        Ok(()) => ExitCode::from(0),
        Err(error) => {
            eprintln!("qpdf-ctest: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: &[std::ffi::OsString]) -> Result<()> {
    if args.len() == 2 && args[1] == "--version" {
        println!("qpdf-ctest version {}", flpdf::qpdf_version());
        return Ok(());
    }
    if args.len() != 5 || args[1] != "19" {
        return Err(flpdf::Error::Unsupported(
            "usage: qpdf-ctest 19 infile password outfile".to_owned(),
        ));
    }

    let input = PathBuf::from(&args[2]);
    let output = PathBuf::from(&args[4]);
    let password = args[3].to_string_lossy().into_owned().into_bytes();
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            description: input.to_string_lossy().into_owned(),
            ..PdfOpenOptions::default()
        },
    )?;

    // qpdf-ctest.c:test19 calls qpdf_init_write before
    // qpdf_set_deterministic_ID. Preserve that setter order at the Rust
    // writer boundary even though both are configuration operations here.
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(output)?;
    writer.set_deterministic_id(true);
    writer.write()?;
    println!("C test 19 done");
    Ok(())
}
