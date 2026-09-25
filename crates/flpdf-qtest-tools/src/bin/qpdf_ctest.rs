//! Portable Rust process adapter for the qpdf C test helper's selected cases.
//!
//! qpdf's `qpdf-ctest.c:test19` (`qpdf-ctest.c:435-442`), test20
//! (`qpdf-ctest.c:445-455`), and JSON tests 42–47
//! (`qpdf-ctest.c:1252-1320`) intentionally test C API lifecycles rather than
//! requiring callers to link a C symbol. Keep this adapter at the qtest-tools
//! process boundary; the PDF read/write responsibilities stay in the canonical
//! `flpdf::Pdf`, `flpdf::PdfWriter`, and `flpdf::document_json` APIs. JSON
//! tests 46/47 in particular port `qpdf_write_json` (`libqpdf/qpdf-c.cc:1924-
//! 1952`), which calls `QPDF::writeJSON` directly and never touches
//! `QPDFJob` (`rg -n 'QPDFJob' libqpdf/qpdf-c.cc` is 0 hits) — so this
//! adapter routes those two cases through `flpdf::document_json::write_json`
//! rather than the `QPDFJob` JSON job API used by the `--json-output` CLI
//! path.

use flpdf::json_inspect::{DecodeLevel as JsonDecodeLevel, JsonObjectSelector, StreamDataMode};
use flpdf::pipeline::PlOStream;
use flpdf::{
    document_json, DecodeLevel, EncryptMethod, EncryptParams, EncryptedError, Error, Pdf,
    PdfOpenOptions, PdfWriter, Permissions, PermissionsConfig, PrintPermission, QpdfErrorCode,
    QpdfExc, R2PermissionsConfig, Result,
};
use std::env;
use std::fs::File;
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Extract the raw password bytes from an argv entry the way qpdf's C API
/// receives them: as the platform's native `argv[]` bytes, with no forced
/// UTF-8 validation. On Unix, `OsStr` already holds those bytes directly. On
/// Windows, narrow C `main` receives the wide process arguments converted to
/// the active ANSI code page. A lossy UTF-8 conversion changes those bytes and
/// can reject a legacy single-byte-encoded password before authentication.
#[cfg(unix)]
fn password_bytes(password_arg: &std::ffi::OsStr) -> Result<Vec<u8>> {
    Ok(std::os::unix::ffi::OsStrExt::as_bytes(password_arg).to_vec())
}

#[cfg(windows)]
fn password_bytes(password_arg: &std::ffi::OsStr) -> Result<Vec<u8>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Globalization::{WideCharToMultiByte, CP_ACP};

    let wide: Vec<u16> = password_arg.encode_wide().collect();
    if wide.is_empty() {
        return Ok(Vec::new());
    }
    let wide_len = i32::try_from(wide.len()).map_err(|_| {
        Error::Internal("password argument is too long for Windows conversion".into())
    })?;

    // SAFETY: `wide` is live for `wide_len` UTF-16 units; a null output pointer
    // with a zero size is the documented `WideCharToMultiByte` sizing call.
    let required = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            0,
            wide.as_ptr(),
            wide_len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if required == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut output = vec![0_u8; required as usize];
    // SAFETY: `output` has the exact size returned by the sizing call, and the
    // same live input pointer and length are used for both conversion calls.
    let written = unsafe {
        WideCharToMultiByte(
            CP_ACP,
            0,
            wide.as_ptr(),
            wide_len,
            output.as_mut_ptr(),
            required,
            std::ptr::null(),
            std::ptr::null_mut(),
        )
    };
    if written == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if written != required {
        return Err(Error::Internal(
            "Windows password conversion returned an unexpected byte count".into(),
        ));
    }
    Ok(output)
}

#[cfg(not(any(unix, windows)))]
fn password_bytes(password_arg: &std::ffi::OsStr) -> Result<Vec<u8>> {
    Ok(password_arg.to_string_lossy().into_owned().into_bytes())
}

#[cfg(unix)]
fn path_description(path: &std::path::Path) -> Vec<u8> {
    std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()).to_vec()
}

#[cfg(not(unix))]
fn path_description(path: &std::path::Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

/// Render a filesystem error at qpdf's `QPDFSystemError::createWhat` boundary
/// (`libqpdf/QPDFSystemError.cc:13-29`). Rust's `io::Error` display carries an
/// OS-error suffix on some platforms, while qpdf uses the C-runtime spelling.
fn qpdf_file_io_source_message(source: &std::io::Error) -> String {
    let message = match source.kind() {
        std::io::ErrorKind::NotFound => Some("No such file or directory"),
        std::io::ErrorKind::PermissionDenied => Some("Permission denied"),
        std::io::ErrorKind::AlreadyExists => Some("File exists"),
        std::io::ErrorKind::InvalidInput => Some("Invalid argument"),
        std::io::ErrorKind::IsADirectory => Some("Is a directory"),
        std::io::ErrorKind::NotADirectory => Some("Not a directory"),
        _ => None,
    };
    if let Some(message) = message {
        return message.to_owned();
    }
    let rendered = source.to_string();
    source
        .raw_os_error()
        .and_then(|code| rendered.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&rendered)
        .to_owned()
}

fn qpdf_file_io_message(
    operation: &str,
    path: &std::path::Path,
    source: &std::io::Error,
) -> Vec<u8> {
    let mut message = operation.as_bytes().to_vec();
    message.push(b' ');
    message.extend_from_slice(&path_description(path));
    message.extend_from_slice(b": ");
    message.extend_from_slice(qpdf_file_io_source_message(source).as_bytes());
    message
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
    if args.len() < 5 {
        return Err(flpdf::Error::Unsupported(
            "usage: qpdf-ctest n infile password outfile".to_owned(),
        ));
    }

    let extra_arg = args.get(5).map(|arg| arg.as_os_str());
    match args[1].to_str() {
        Some("1") => run_test1(&args[2], &args[3]),
        Some("2") => run_test2(&args[2], &args[3], &args[4]),
        Some("10") => run_test10(&args[2], &args[3], &args[4]),
        Some("11") => run_test11(&args[2], &args[3], &args[4]),
        Some("12") => run_test12(&args[2], &args[3], &args[4]),
        Some("13") => run_test13(&args[2], &args[3], &args[4]),
        Some("15") => run_test15(&args[2], &args[3], &args[4]),
        Some("17") => run_test17(&args[2], &args[3], &args[4]),
        Some("18") => run_test18(&args[2], &args[3], &args[4]),
        Some("19") => run_test19(&args[2], &args[3], &args[4]),
        Some("20") => run_test20(&args[2], &args[3], &args[4]),
        Some("22") => run_test22(&args[2], &args[3], &args[4]),
        Some("42") => run_test42(&args[2], &args[4]),
        Some("43") => run_test43(&args[2], &args[4]),
        Some("44") => run_test44(
            &args[2],
            &args[3],
            &args[4],
            required_extra_arg("44", extra_arg)?,
        ),
        Some("45") => run_test45(
            &args[2],
            &args[3],
            &args[4],
            required_extra_arg("45", extra_arg)?,
        ),
        Some("46") => run_test46(&args[2], &args[3], &args[4]),
        Some("47") => run_test47(
            &args[2],
            &args[3],
            &args[4],
            required_extra_arg("47", extra_arg)?,
        ),
        Some(test_number) => Err(flpdf::Error::Unsupported(format!(
            "invalid test number {test_number}"
        ))),
        None => Err(flpdf::Error::Unsupported("invalid test number".to_owned())),
    }
}

fn required_extra_arg<'a>(
    test_number: &str,
    value: Option<&'a std::ffi::OsStr>,
) -> Result<&'a std::ffi::OsStr> {
    value.ok_or_else(|| {
        Error::Unsupported(format!(
            "usage: qpdf-ctest test {test_number} requires an extra JSON argument"
        ))
    })
}

fn read_options(input: &std::path::Path, password: Vec<u8>) -> PdfOpenOptions {
    PdfOpenOptions {
        password,
        // The qpdf C API authenticates with one password candidate. The
        // alternate encoding retry loop belongs to QPDFJob, not qpdf-c.
        suppress_password_recovery: true,
        description: path_description(input),
        ..PdfOpenOptions::default()
    }
}

fn open_input(input_arg: &std::ffi::OsStr, password_arg: &std::ffi::OsStr) -> Result<Pdf<File>> {
    let input = PathBuf::from(input_arg);
    let password = password_bytes(password_arg)?;
    Pdf::open_with_options(File::open(&input)?, read_options(&input, password))
}

/// Print qpdf-ctest's portable error object projection. This is the Rust
/// process equivalent of qpdf-ctest.c:35-68 (`print_error` at `:35-43`,
/// `report_errors` at `:45-68`): C API callers observe the
/// structured QPDFExc fields after qpdf has already collected the warning
/// objects. The C ABI itself remains outside this crate.
fn write_c_api_error(output: &mut impl Write, label: &[u8], error: &QpdfExc) -> Result<()> {
    output.write_all(label)?;
    output.write_all(b": ")?;
    output.write_all(error.what_bytes())?;
    output.write_all(b"\n  code: ")?;
    write!(output, "{}", error.get_error_code() as i32)?;
    output.write_all(b"\n  file: ")?;
    output.write_all(error.get_filename())?;
    output.write_all(b"\n  pos: ")?;
    write!(output, "{}", error.get_file_position())?;
    output.write_all(b"\n  text: ")?;
    output.write_all(error.get_message_detail())?;
    output.write_all(b"\n")?;
    Ok(())
}

fn qpdf_exception_from_error(input: &Path, error: &Error) -> QpdfExc {
    match error {
        Error::QpdfExc(error) => error.clone(),
        Error::OpenFailure { source, .. } => qpdf_exception_from_error(input, source),
        Error::FileIo {
            operation,
            path,
            source,
        } => QpdfExc::new(
            QpdfErrorCode::System,
            b"",
            b"",
            0,
            qpdf_file_io_message(operation, path, source),
        ),
        Error::Parse { offset, message } => QpdfExc::new(
            QpdfErrorCode::DamagedPdf,
            path_description(input),
            b"",
            i64::try_from(*offset).unwrap_or(i64::MAX),
            message.as_bytes(),
        ),
        Error::Encrypted(EncryptedError::BadPassword) => QpdfExc::new(
            QpdfErrorCode::Password,
            path_description(input),
            b"",
            0,
            b"invalid password",
        ),
        // qpdf throws `QPDFExc(qpdf_e_unsupported, ...)` for an unsupported
        // `/Filter` and for an unsupported `/R`//`/V` pair, and
        // `damagedPDF` (`qpdf_e_damaged_pdf`) for a malformed `/Encrypt`
        // dictionary (`QPDF_encryption.cc:748-794`). Only a rejected password
        // is `qpdf_e_password`.
        Error::Encrypted(error @ EncryptedError::UnsupportedHandler { .. }) => QpdfExc::new(
            QpdfErrorCode::Unsupported,
            path_description(input),
            b"",
            0,
            error.to_string().as_bytes(),
        ),
        Error::Encrypted(error @ EncryptedError::Malformed { .. }) => QpdfExc::new(
            QpdfErrorCode::DamagedPdf,
            path_description(input),
            b"",
            0,
            error.to_string().as_bytes(),
        ),
        Error::Unsupported(message) => QpdfExc::new(
            QpdfErrorCode::Unsupported,
            path_description(input),
            b"",
            0,
            message.as_bytes(),
        ),
        // The remaining arms are the ones qpdf never raises as a `QPDFExc`.
        // `trap_errors` rebuilds them from a caught `std::runtime_error` or
        // `std::exception` as `QPDFExc(code, "", "", 0, e.what())`
        // (`qpdf-c.cc:77-82`), so the location fields stay **empty** and the
        // whole `what()` becomes the detail. `QPDFSystemError::what()` already
        // carries the filename, so repeating it here would print the path
        // twice and report a filename qpdf leaves blank.
        Error::SystemBytes(message) => QpdfExc::new(QpdfErrorCode::System, b"", b"", 0, message),
        Error::System(message) => {
            QpdfExc::new(QpdfErrorCode::System, b"", b"", 0, message.as_bytes())
        }
        // `Error::Internal` is qpdf's `std::logic_error` family, which the C
        // API's `catch (std::exception&)` arm reports as `qpdf_e_internal`
        // rather than `qpdf_e_system` (`qpdf-c.cc:80-82`).
        Error::Internal(message) => {
            QpdfExc::new(QpdfErrorCode::Internal, b"", b"", 0, message.as_bytes())
        }
        Error::Io(error) => QpdfExc::new(
            QpdfErrorCode::System,
            b"",
            b"",
            0,
            error.to_string().as_bytes(),
        ),
        other => QpdfExc::new(
            QpdfErrorCode::System,
            b"",
            b"",
            0,
            other.to_string().as_bytes(),
        ),
    }
}

fn write_pdf_diagnostics<R: Read + Seek>(pdf: &Pdf<R>, output: &mut impl Write) -> Result<()> {
    for warning in pdf.repair_diagnostics().entries() {
        write_c_api_error(output, b"warning", warning)?;
    }
    Ok(())
}

fn write_open_error_report(input: &Path, error: &Error, output: &mut impl Write) -> Result<()> {
    if let Some((source, diagnostics)) = error.open_failure() {
        for warning in diagnostics.entries() {
            write_c_api_error(output, b"warning", warning)?;
        }
        let terminal = qpdf_exception_from_error(input, source);
        write_c_api_error(output, b"error", &terminal)?;
    } else {
        let terminal = qpdf_exception_from_error(input, error);
        write_c_api_error(output, b"error", &terminal)?;
    }
    Ok(())
}

/// Run qpdf's C API authentication/error case (`qpdf-ctest.c:test02`).
///
/// The C API reports a failed read through its error object and still returns
/// process success after the helper prints that object. Preserve that
/// distinction from the Rust process adapter's own fatal errors.
fn run_test2(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let password = password_bytes(password_arg)?;
    // qpdf's `qpdf_read` catches both its input-open and parse exceptions in
    // `trap_errors` (`qpdf-c.cc:68-89,266-282`). Keep the filesystem open in
    // the same reportable result instead of letting `?` terminate the Rust
    // process before the C API projection runs.
    let result = File::open(&input)
        .map_err(|source| Error::FileIo {
            operation: "open",
            path: input.clone(),
            source,
        })
        .and_then(|file| {
            Pdf::open_with_options(
                file,
                PdfOpenOptions {
                    password,
                    suppress_password_recovery: true,
                    suppress_warnings: true,
                    description: path_description(&input),
                    ..PdfOpenOptions::default()
                },
            )
        });
    match result {
        Ok(mut pdf) => {
            let output = PathBuf::from(output_arg);
            let write_result = {
                let mut writer = PdfWriter::new(&mut pdf);
                writer.set_output_file(&output).and_then(|_| {
                    writer.set_static_id(true);
                    writer.write()
                })
            };
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            if let Err(error) = write_result {
                // `report_errors()` drains warnings even when init_write or
                // write fails (`qpdf-ctest.c:44-68,127-136`).
                write_pdf_diagnostics(&pdf, &mut stdout)?;
                let terminal = qpdf_exception_from_error(&input, &error);
                write_c_api_error(&mut stdout, b"error", &terminal)?;
            } else {
                write_pdf_diagnostics(&pdf, &mut stdout)?;
            }
            writeln!(stdout, "C test 2 done")?;
            Ok(())
        }
        Err(error) => {
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            write_open_error_report(&input, &error, &mut stdout)?;
            writeln!(stdout, "C test 2 done")?;
            Ok(())
        }
    }
}

/// Run qpdf-ctest.c:test10. The raw C API disables recovery before reading,
/// then reports the resulting error object and returns success from the
/// helper process (`qpdf-ctest.c:259-265`). Unlike `test02`, `test10` never
/// calls `qpdf_set_suppress_warnings` (its only use is `qpdf-ctest.c:163`),
/// so any warning raised before the terminal error reaches both the live
/// logger and the replayed report.
fn run_test10(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    _output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let password = password_bytes(password_arg)?;
    let result = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            repair: false,
            password,
            suppress_password_recovery: true,
            description: path_description(&input),
            ..PdfOpenOptions::default()
        },
    );
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    match result {
        Ok(pdf) => write_pdf_diagnostics(&pdf, &mut stdout)?,
        Err(error) => write_open_error_report(&input, &error, &mut stdout)?,
    }
    writeln!(stdout, "C test 10 done")?;
    Ok(())
}

fn low_print_permissions() -> PermissionsConfig {
    PermissionsConfig {
        print: PrintPermission::Low,
        ..PermissionsConfig::default()
    }
}

fn write_with_encryption(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
    params: EncryptParams,
    static_aes_iv: bool,
) -> Result<()> {
    let output = PathBuf::from(output_arg);
    let mut pdf = open_input(input_arg, password_arg)?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(output)?;
    writer.set_static_id(true);
    if static_aes_iv {
        writer.set_static_aes_iv(true);
    }
    writer.set_encryption_parameters(params);
    writer.write()
}

/// qpdf C API test11: V=1/R=2 RC4-40 with the four legacy permission bits.
fn run_test11(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let params = EncryptParams {
        method: EncryptMethod::V1Rc440,
        user_password: b"user1".to_vec(),
        owner_password: b"owner1".to_vec(),
        permissions: PermissionsConfig::default(),
        r2_permissions: R2PermissionsConfig {
            print: false,
            modify: true,
            extract: true,
            annotate: true,
        },
        encrypt_metadata: true,
    };
    write_with_encryption(input_arg, password_arg, output_arg, params, false)?;
    println!("C test 11 done");
    Ok(())
}

/// qpdf C API test12: V=2/R=3 RC4-128 with full permissions and low printing.
fn run_test12(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let params = EncryptParams {
        method: EncryptMethod::V2Rc4128,
        user_password: b"user2".to_vec(),
        owner_password: b"owner2".to_vec(),
        permissions: low_print_permissions(),
        r2_permissions: R2PermissionsConfig::default(),
        encrypt_metadata: true,
    };
    write_with_encryption(input_arg, password_arg, output_arg, params, false)?;
    println!("C test 12 done");
    Ok(())
}

/// qpdf C API test13: report the recovered user password, then write a
/// decrypted copy with encryption preservation disabled.
fn run_test13(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let mut pdf = open_input(input_arg, password_arg)?;
    let user_password = pdf
        .trimmed_user_password()
        .ok_or_else(|| Error::Internal("test13 input is not encrypted".to_owned()))?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(b"user password: ")?;
    stdout.write_all(&user_password)?;
    stdout.write_all(b"\n")?;
    drop(stdout);

    let output = PathBuf::from(output_arg);
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(output)?;
    writer.set_static_id(true);
    writer.set_preserve_encryption(false);
    writer.write()?;
    println!("C test 13 done");
    Ok(())
}

/// qpdf C API test15: V=4/R=4 AES-128 with a static IV and low printing.
fn run_test15(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let params = EncryptParams {
        method: EncryptMethod::V4Aes128,
        user_password: b"user2".to_vec(),
        owner_password: b"owner2".to_vec(),
        permissions: low_print_permissions(),
        r2_permissions: R2PermissionsConfig::default(),
        encrypt_metadata: true,
    };
    write_with_encryption(input_arg, password_arg, output_arg, params, true)?;
    println!("C test 15 done");
    Ok(())
}

/// qpdf C API test17: V=5/R=5 AES-256 with static AES IV.
fn run_test17(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let params = EncryptParams {
        method: EncryptMethod::V5R5Aes256,
        user_password: b"user3".to_vec(),
        owner_password: b"owner3".to_vec(),
        permissions: low_print_permissions(),
        r2_permissions: R2PermissionsConfig::default(),
        encrypt_metadata: true,
    };
    write_with_encryption(input_arg, password_arg, output_arg, params, true)?;
    println!("C test 17 done");
    Ok(())
}

/// qpdf C API test18: V=5/R=6 AES-256 with static AES IV.
fn run_test18(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let params = EncryptParams {
        method: EncryptMethod::V5R6Aes256,
        user_password: b"user4".to_vec(),
        owner_password: b"owner4".to_vec(),
        permissions: low_print_permissions(),
        r2_permissions: R2PermissionsConfig::default(),
        encrypt_metadata: true,
    };
    write_with_encryption(input_arg, password_arg, output_arg, params, true)?;
    println!("C test 18 done");
    Ok(())
}

/// Run qpdf's portable metadata observation case (`qpdf-ctest.c:test01`).
///
/// The C helper intentionally ignores `outfile`: it only reads the input and
/// reports the document projections exposed by qpdf's C API. Keep the output
/// byte-oriented so a recovered user password is not passed through UTF-8
/// replacement before it reaches the harness.
fn run_test1(input_arg: &std::ffi::OsStr, password_arg: &std::ffi::OsStr) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let password = password_bytes(password_arg)?;
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            // qpdf's C API `qpdf_read` authenticates with a single attempt;
            // the alternate-encoding retry loop is QPDFJob-only
            // (`libqpdf/QPDFJob.cc:1744`, gated on `m->suppress_password_recovery`)
            // and is never reached through the raw C API this test targets.
            suppress_password_recovery: true,
            description: path_description(&input),
            ..PdfOpenOptions::default()
        },
    )?;

    // Match qpdf-ctest.c:test01's accessor order: version, extension level,
    // linearization, encryption, then the encrypted-only projections.
    let version = pdf.version().to_owned();
    // `qpdf_get_pdf_extension_level` (`qpdf-c.cc:319-324`) wraps
    // `QPDF::getExtensionLevel`, which clamps to `i32` range and warns on
    // overflow (`libqpdf/QPDF.cc:2328-2346`); the raw `adobe_extension_level`
    // accessor this replaced kept the full 64-bit value and never warned.
    let extension_level = pdf.get_extension_level()?;
    let linearized = pdf.is_linearized()?;
    let encrypted = pdf.is_encrypted();
    let encryption_revision = encrypted
        .then(|| {
            pdf.encryption_revision().ok_or_else(|| {
                flpdf::Error::Internal("encrypted PDF has no encryption revision".to_owned())
            })
        })
        .transpose()?;
    let user_password = encrypted
        .then(|| {
            pdf.trimmed_user_password().ok_or_else(|| {
                flpdf::Error::Internal("encrypted PDF has no user password state".to_owned())
            })
        })
        .transpose()?;
    let permissions = encrypted
        .then(|| {
            pdf.permissions().ok_or_else(|| {
                flpdf::Error::Internal("encrypted PDF has no permissions".to_owned())
            })
        })
        .transpose()?;

    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    writeln!(stdout, "version: {version}")?;
    if extension_level > 0 {
        // qpdf-ctest.c:139-141 calls `qpdf_get_pdf_extension_level` a second
        // time here (redundantly) to print it; each call independently
        // clamps and warns, so a value that needs clamping is warned about
        // twice. Re-reading here instead of reusing `extension_level`
        // reproduces that second warning.
        let printed_extension_level = pdf.get_extension_level()?;
        writeln!(stdout, "extension level: {printed_extension_level}")?;
    }
    writeln!(stdout, "linearized: {}", u8::from(linearized))?;
    writeln!(stdout, "encrypted: {}", u8::from(encrypted))?;

    if let (Some(revision), Some(user_password), Some(permissions)) =
        (encryption_revision, user_password, permissions)
    {
        write_encrypted_observations(&mut stdout, revision, &user_password, &permissions)?;
    }
    write_pdf_diagnostics(&pdf, &mut stdout)?;
    writeln!(stdout, "C test 1 done")?;
    Ok(())
}

fn write_encrypted_observations(
    output: &mut impl Write,
    revision: i64,
    user_password: &[u8],
    permissions: &Permissions,
) -> Result<()> {
    output.write_all(b"user password: ")?;
    output.write_all(user_password)?;
    output.write_all(b"\n")?;

    let raw = permissions.raw();
    let bit = |number: u32| (raw as u32) & (1u32 << (number - 1)) != 0;
    let accessibility = if revision < 3 { bit(5) } else { bit(10) };
    let extract_all = bit(5);
    let print_low = bit(3);
    let print_high = print_low && (revision < 3 || bit(12));
    let modify_assembly = if revision < 3 { bit(4) } else { bit(11) };
    let modify_forms = if revision < 3 { bit(6) } else { bit(9) };
    let modify_annotations = bit(6);
    let modify_other = bit(4);
    let modify_all =
        modify_other && modify_annotations && (revision < 3 || (modify_forms && modify_assembly));

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

fn write_static_id_pdf<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let mut writer = PdfWriter::new(pdf);
    writer.set_output_file(PathBuf::from(output_arg))?;
    writer.set_static_id(true);
    writer.write()
}

fn run_test42(input_arg: &std::ffi::OsStr, output_arg: &std::ffi::OsStr) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let mut pdf = Pdf::create_from_json_file(&input)?;
    write_static_id_pdf(&mut pdf, output_arg)?;
    println!("C test 42 done");
    Ok(())
}

fn run_test43(input_arg: &std::ffi::OsStr, output_arg: &std::ffi::OsStr) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let json = std::fs::read(&input)?;
    let mut pdf = Pdf::create_from_json(Cursor::new(json), path_description(&input))?;
    write_static_id_pdf(&mut pdf, output_arg)?;
    println!("C test 43 done");
    Ok(())
}

fn run_test44(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
    update_arg: &std::ffi::OsStr,
) -> Result<()> {
    let update = PathBuf::from(update_arg);
    let mut pdf = open_input(input_arg, password_arg)?;
    pdf.update_from_json_file(&update)?;
    write_static_id_pdf(&mut pdf, output_arg)?;
    println!("C test 44 done");
    Ok(())
}

fn run_test45(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
    update_arg: &std::ffi::OsStr,
) -> Result<()> {
    let update = PathBuf::from(update_arg);
    let json = std::fs::read(&update)?;
    let mut pdf = open_input(input_arg, password_arg)?;
    pdf.update_from_json(Cursor::new(json), path_description(&update))?;
    write_static_id_pdf(&mut pdf, output_arg)?;
    println!("C test 45 done");
    Ok(())
}

/// Serialize `pdf` to `output_arg` the way `qpdf_write_json`
/// (`libqpdf/qpdf-c.cc:1924-1952`) does: a direct `QPDF::writeJSON` call,
/// with no `QPDFJob` in the path. `document_json::write_json` walks every
/// object (`QPDF_json.cc:900-925`), dereferencing it for the first time if
/// unresolved, so a malformed object can raise the same lazy-resolution
/// warning any other first resolution would; the caller drains those with
/// [`write_pdf_diagnostics`] afterward, matching `report_errors()`
/// (`qpdf-ctest.c:45-68`) and this file's other resolution-triggering
/// tests (`run_test2`, `run_test10`).
fn write_json_test_output<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    output_arg: &std::ffi::OsStr,
    decode_level: JsonDecodeLevel,
    stream_mode: &StreamDataMode,
    objects: &[JsonObjectSelector],
) -> Result<()> {
    let output = PathBuf::from(output_arg);
    let file = File::create(&output)?;
    let mut sink = PlOStream::new("json output", file);
    document_json::write_json(pdf, 2, &mut sink, decode_level, stream_mode, objects)?;
    Ok(())
}

fn run_test46(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let mut pdf = open_input(input_arg, password_arg)?;
    write_json_test_output(
        &mut pdf,
        output_arg,
        JsonDecodeLevel::None,
        &StreamDataMode::Inline,
        &[],
    )?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    write_pdf_diagnostics(&pdf, &mut stdout)?;
    writeln!(stdout, "C test 46 done")?;
    Ok(())
}

fn run_test47(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
    prefix_arg: &std::ffi::OsStr,
) -> Result<()> {
    let prefix = path_description(Path::new(prefix_arg));
    let objects = [
        JsonObjectSelector::Object {
            number: 4,
            generation: 0,
        },
        JsonObjectSelector::Trailer,
    ];
    let mut pdf = open_input(input_arg, password_arg)?;
    write_json_test_output(
        &mut pdf,
        output_arg,
        JsonDecodeLevel::Specialized,
        &StreamDataMode::File { prefix },
        &objects,
    )?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    write_pdf_diagnostics(&pdf, &mut stdout)?;
    writeln!(stdout, "C test 47 done")?;
    Ok(())
}

fn run_test19(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let input = PathBuf::from(input_arg);
    let output = PathBuf::from(output_arg);
    let password = password_bytes(password_arg)?;
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            // See run_test1's identical setting: qpdf's C API authenticates
            // with a single attempt, with no QPDFJob-only recovery retry.
            suppress_password_recovery: true,
            description: path_description(&input),
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
    let password = password_bytes(password_arg)?;
    let mut pdf = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            // The raw qpdf C API performs a single authentication attempt;
            // password recovery is a QPDFJob-only policy.
            suppress_password_recovery: true,
            description: path_description(&input),
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

/// Run qpdf's C API writer configuration case (`qpdf-ctest.c:test22`).
///
/// Keep the setter order from qpdf's helper: static IDs and AES IVs are set
/// before disabling stream compression and enabling the newline policy
/// (`qpdf/qpdf-ctest.c:470-479`).
fn run_test22(
    input_arg: &std::ffi::OsStr,
    password_arg: &std::ffi::OsStr,
    output_arg: &std::ffi::OsStr,
) -> Result<()> {
    let mut pdf = open_input(input_arg, password_arg)?;
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_output_file(PathBuf::from(output_arg))?;
    writer.set_static_id(true);
    writer.set_static_aes_iv(true);
    writer.set_compress_streams(false);
    writer.set_newline_before_endstream(true);
    writer.write()?;
    println!("C test 22 done");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run;
    use std::ffi::OsString;

    #[cfg(unix)]
    use super::password_bytes;
    #[cfg(windows)]
    use super::password_bytes;
    #[cfg(windows)]
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    #[cfg(windows)]
    use std::os::windows::ffi::OsStrExt;
    #[cfg(windows)]
    use windows_sys::Win32::Globalization::{GetACP, WideCharToMultiByte, CP_ACP, CP_UTF8};

    #[cfg(unix)]
    #[test]
    fn password_bytes_preserves_non_utf8_argv_bytes_on_unix() {
        // qpdf's C API receives argv as raw bytes and never validates them as
        // UTF-8, so a legacy single-byte-encoded password byte like 0xe9
        // (é in Latin-1) must survive unchanged. `to_string_lossy()` would
        // replace it with the 3-byte U+FFFD sequence instead.
        let raw = [b'p', b'w', 0xe9, b'!'];
        let arg = std::ffi::OsStr::from_bytes(&raw);
        assert_eq!(
            password_bytes(arg).expect("Unix argv conversion cannot fail"),
            raw.to_vec()
        );
    }

    #[cfg(windows)]
    fn windows_c_main_argv_bytes(value: &OsStr) -> Vec<u8> {
        let wide: Vec<u16> = value.encode_wide().collect();
        assert!(!wide.is_empty());
        let wide_len = i32::try_from(wide.len()).unwrap();
        // SAFETY: the input pointer is valid for wide_len UTF-16 code units;
        // a null output pointer with a zero size is the documented sizing call.
        let required = unsafe {
            WideCharToMultiByte(
                CP_ACP,
                0,
                wide.as_ptr(),
                wide_len,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        assert!(required > 0);
        let mut output = vec![0_u8; required as usize];
        // SAFETY: output has required bytes, and the same live input pointer and
        // length are passed as in the sizing call above.
        let written = unsafe {
            WideCharToMultiByte(
                CP_ACP,
                0,
                wide.as_ptr(),
                wide_len,
                output.as_mut_ptr(),
                required,
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(written, required);
        output
    }

    #[cfg(windows)]
    #[test]
    fn password_bytes_matches_the_narrow_c_main_argv_encoding_on_windows() {
        let password = OsStr::new("pw-é!");
        let expected = windows_c_main_argv_bytes(password);
        let actual = password_bytes(password).expect("Windows ACP conversion should succeed");
        assert_eq!(actual, expected);
        assert!(password_bytes(OsStr::new(""))
            .expect("empty Windows argv conversion should succeed")
            .is_empty());
        // SAFETY: GetACP takes no arguments and has no pointer preconditions.
        let active_code_page = unsafe { GetACP() };
        if active_code_page != CP_UTF8 {
            assert_ne!(
                actual,
                password.to_string_lossy().into_owned().into_bytes(),
                "non-UTF-8 ACP argv bytes must not be replaced with lossy UTF-8"
            );
        }
    }

    #[test]
    fn run_rejects_missing_common_arguments() {
        let args = vec![OsString::from("qpdf-ctest"), OsString::from("42")];
        let error = run(&args).expect_err("qpdf-ctest requires the common arguments");
        assert!(error.to_string().contains("usage: qpdf-ctest"));
    }

    #[test]
    fn run_rejects_missing_json_extra_argument() {
        let args = vec![
            OsString::from("qpdf-ctest"),
            OsString::from("44"),
            OsString::from("input.pdf"),
            OsString::new(),
            OsString::from("output.pdf"),
        ];
        let error = run(&args).expect_err("test44 requires its update JSON argument");
        assert!(error
            .to_string()
            .contains("requires an extra JSON argument"));
    }
}
