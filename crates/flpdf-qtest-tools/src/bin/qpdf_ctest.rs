//! Portable Rust translation of the qpdf C test helper's deterministic-ID case.
//!
//! qpdf's `qpdf-ctest.c:test19` (`qpdf-ctest.c:435-442`) intentionally tests
//! the C API lifecycle rather than a C symbol. Its observable sequence is:
//! read the input, initialize a writer for the output, enable deterministic
//! IDs, write, report errors, and print `C test 19 done`. Keep this adapter at
//! the qtest-tools process boundary; the PDF read/write responsibilities stay
//! in the canonical `flpdf::Pdf` and `flpdf::PdfWriter` APIs.

use flpdf::{EncryptionInfo, Pdf, PdfOpenOptions, PdfWriter, Result};
use std::env;
use std::fs::File;
use std::io::Write;
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
    if args.len() != 5 {
        return Err(flpdf::Error::Unsupported(
            "usage: qpdf-ctest 19 infile password outfile".to_owned(),
        ));
    }

    match args[1].to_str() {
        Some("1") => run_test1(&args[2], &args[3]),
        Some("19") => run_test19(&args[2], &args[3], &args[4]),
        _ => Err(flpdf::Error::Unsupported(
            "usage: qpdf-ctest 19 infile password outfile".to_owned(),
        )),
    }
}

/// Run qpdf's portable metadata observation case (`qpdf-ctest.c:test01`).
///
/// The C helper intentionally ignores `outfile`: it only reads the input and
/// reports the document projections exposed by qpdf's C API. Keep the output
/// byte-oriented so a recovered user password is not passed through UTF-8
/// replacement before it reaches the harness.
fn run_test1(input_arg: &std::ffi::OsStr, password_arg: &std::ffi::OsStr) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let password = password_arg.to_string_lossy().into_owned().into_bytes();
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            description: input.to_string_lossy().into_owned(),
            ..PdfOpenOptions::default()
        },
    )?;

    // Match qpdf-ctest.c:test01's accessor order: version, extension level,
    // linearization, encryption, then the encrypted-only projections.
    let version = pdf.version().to_owned();
    let extension_level = pdf.adobe_extension_level();
    let linearized = pdf.is_linearized()?;
    let encrypted = pdf.is_encrypted();
    let encryption_info = if encrypted {
        Some(pdf.encryption_info()?.ok_or_else(|| {
            flpdf::Error::Internal("encrypted PDF has no encryption info".to_owned())
        })?)
    } else {
        None
    };

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "version: {version}")?;
    if let Some(extension_level) = extension_level.filter(|level| *level > 0) {
        writeln!(stdout, "extension level: {extension_level}")?;
    }
    writeln!(stdout, "linearized: {}", u8::from(linearized))?;
    writeln!(stdout, "encrypted: {}", u8::from(encrypted))?;

    if let Some(info) = encryption_info.as_ref() {
        write_encrypted_observations(&mut stdout, info)?;
    }
    writeln!(stdout, "C test 1 done")?;
    Ok(())
}

fn write_encrypted_observations(output: &mut impl Write, info: &EncryptionInfo) -> Result<()> {
    output.write_all(b"user password: ")?;
    output.write_all(&info.user_password)?;
    output.write_all(b"\n")?;

    let raw = info.permissions.raw();
    let bit = |number: u32| (raw as u32) & (1u32 << (number - 1)) != 0;
    let accessibility = if info.r < 3 { bit(5) } else { bit(10) };
    let extract_all = bit(5);
    let print_low = bit(3);
    let print_high = print_low && (info.r < 3 || bit(12));
    let modify_assembly = if info.r < 3 { bit(4) } else { bit(11) };
    let modify_forms = if info.r < 3 { bit(6) } else { bit(9) };
    let modify_annotations = bit(6);
    let modify_other = bit(4);
    let modify_all =
        modify_other && modify_annotations && (info.r < 3 || (modify_forms && modify_assembly));

    for (label, value) in [
        ("extract for accessibility", accessibility),
        ("extract for any purpose", extract_all),
        ("print low resolution", print_low),
        ("print high resolution", print_high),
        ("modify document assembly", modify_assembly),
        ("modify forms", modify_forms),
        ("modify annotations", modify_annotations),
        ("modify other", modify_other),
        ("modify anything", modify_all),
    ] {
        writeln!(output, "{label}: {}", u8::from(value))?;
    }
    Ok(())
}

fn run_test19(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let output = PathBuf::from(output_arg);
    let password = password_arg.to_string_lossy().into_owned().into_bytes();
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
