//! Portable Rust process adapter for the qpdf C test helper's selected cases.
//!
//! qpdf's `qpdf-ctest.c:test19` (`qpdf-ctest.c:435-442`) and test20
//! (`qpdf-ctest.c:445-455`) intentionally test C API lifecycles rather than
//! requiring callers to link a C symbol. Keep this adapter at the qtest-tools
//! process boundary; the PDF read/write responsibilities stay in the canonical
//! `flpdf::Pdf` and `flpdf::PdfWriter` APIs.

use flpdf::{DecodeLevel, EncryptionInfo, Pdf, PdfOpenOptions, PdfWriter, Result};
use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

/// Extract the raw password bytes from an argv entry the way qpdf's C API
/// receives them: as the platform's native `argv[]` bytes, with no forced
/// UTF-8 validation. On Unix, `OsStr` already holds those bytes directly; a
/// lossy `to_string_lossy()` conversion would replace any non-UTF-8 byte
/// with U+FFFD before authentication, rejecting a legacy single-byte-encoded
/// password (e.g. Latin-1) that qpdf's `qpdf_read` accepts unchanged.
#[cfg(unix)]
fn password_bytes(password_arg: &std::ffi::OsStr) -> Vec<u8> {
    std::os::unix::ffi::OsStrExt::as_bytes(password_arg).to_vec()
}

#[cfg(not(unix))]
fn password_bytes(password_arg: &std::ffi::OsStr) -> Vec<u8> {
    password_arg.to_string_lossy().into_owned().into_bytes()
}

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
            "usage: qpdf-ctest n infile password outfile".to_owned(),
        ));
    }

    match args[1].to_str() {
        Some("1") => run_test1(&args[2], &args[3]),
        Some("19") => run_test19(&args[2], &args[3], &args[4]),
        Some("20") => run_test20(&args[2], &args[3], &args[4]),
        Some(test_number) => Err(flpdf::Error::Unsupported(format!(
            "invalid test number {test_number}"
        ))),
        None => Err(flpdf::Error::Unsupported("invalid test number".to_owned())),
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
    let password = password_bytes(password_arg);
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            // qpdf's C API `qpdf_read` authenticates with a single attempt;
            // the alternate-encoding retry loop is QPDFJob-only
            // (`libqpdf/QPDFJob.cc:1744`, gated on `m->suppress_password_recovery`)
            // and is never reached through the raw C API this test targets.
            suppress_password_recovery: true,
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
    let password = password_bytes(password_arg);
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            // See run_test1's identical setting: qpdf's C API authenticates
            // with a single attempt, with no QPDFJob-only recovery retry.
            suppress_password_recovery: true,
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

fn run_test20(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let output = PathBuf::from(output_arg);
    let password = password_bytes(password_arg);
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            // The raw qpdf C API performs a single authentication attempt;
            // password recovery is a QPDFJob-only policy.
            suppress_password_recovery: true,
            description: input.to_string_lossy().into_owned(),
            ..PdfOpenOptions::default()
        },
    )?;

    // qpdf-ctest.c:test20 calls qpdf_init_write before all four writer
    // setters. Keep that order at the process adapter boundary so the
    // canonical writer observes the same state transitions.
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(output)?;
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.set_compress_streams(false);
    writer.set_decode_level(DecodeLevel::Specialized);
    writer.write()?;
    println!("C test 20 done");
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::password_bytes;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn password_bytes_preserves_non_utf8_argv_bytes_on_unix() {
        // qpdf's C API receives argv as raw bytes and never validates them as
        // UTF-8, so a legacy single-byte-encoded password byte like 0xe9
        // (é in Latin-1) must survive unchanged. `to_string_lossy()` would
        // replace it with the 3-byte U+FFFD sequence instead.
        let raw = [b'p', b'w', 0xe9, b'!'];
        let arg = std::ffi::OsStr::from_bytes(&raw);
        assert_eq!(password_bytes(arg), raw.to_vec());
    }
}
