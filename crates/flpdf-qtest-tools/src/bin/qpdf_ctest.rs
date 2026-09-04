//! Portable Rust process adapter for the qpdf C test helper's selected cases.
//!
//! qpdf's `qpdf-ctest.c:test19` (`qpdf-ctest.c:435-442`), test20
//! (`qpdf-ctest.c:445-455`), and JSON tests 42–47
//! (`qpdf-ctest.c:1252-1320`) intentionally test C API lifecycles rather than
//! requiring callers to link a C symbol. Keep this adapter at the qtest-tools
//! process boundary; the PDF read/write responsibilities stay in the canonical
//! `flpdf::Pdf`, `flpdf::PdfWriter`, and JSON job APIs.

use flpdf::job::{JsonJobOptions, JsonJobOutput, JsonStreamData, QPDFJob};
use flpdf::json_inspect::{DecodeLevel as JsonDecodeLevel, JsonKey, JsonObjectSelector};
use flpdf::{
    DecodeLevel, EncryptMethod, EncryptParams, EncryptedError, Error, Pdf, PdfOpenOptions,
    PdfWriter, Permissions, PermissionsConfig, PrintPermission, R2PermissionsConfig, Result,
};
use std::env;
use std::fs::File;
use std::io::{Cursor, Read, Seek, Write};
use std::path::{Path, PathBuf};
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

#[cfg(unix)]
fn path_description(path: &std::path::Path) -> Vec<u8> {
    std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()).to_vec()
}

#[cfg(not(unix))]
fn path_description(path: &std::path::Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
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
        Some("11") => run_test11(&args[2], &args[3], &args[4]),
        Some("12") => run_test12(&args[2], &args[3], &args[4]),
        Some("13") => run_test13(&args[2], &args[3], &args[4]),
        Some("15") => run_test15(&args[2], &args[3], &args[4]),
        Some("17") => run_test17(&args[2], &args[3], &args[4]),
        Some("18") => run_test18(&args[2], &args[3], &args[4]),
        Some("19") => run_test19(&args[2], &args[3], &args[4]),
        Some("20") => run_test20(&args[2], &args[3], &args[4]),
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
    let password = password_bytes(password_arg);
    Pdf::open_with_options(File::open(&input)?, read_options(&input, password))
}

fn is_bad_password(error: &Error) -> bool {
    match error {
        Error::Encrypted(EncryptedError::BadPassword) => true,
        Error::OpenFailure { source, .. } => is_bad_password(source),
        _ => false,
    }
}

/// Write `path`'s raw bytes to `output`, matching `qpdf_get_error_filename`'s
/// verbatim `argv` filename (`qpdf-ctest.c:40`) rather than a lossy
/// UTF-8 projection that would replace non-UTF-8 bytes with U+FFFD before
/// the name ever reaches the terminal.
#[cfg(unix)]
fn write_native_path(output: &mut impl Write, path: &std::path::Path) -> Result<()> {
    output.write_all(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_native_path(output: &mut impl Write, path: &std::path::Path) -> Result<()> {
    write!(output, "{}", path.display())?;
    Ok(())
}

fn report_invalid_password(input: &std::path::Path) -> Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    write!(stdout, "error: ")?;
    write_native_path(&mut stdout, input)?;
    writeln!(stdout, ": invalid password")?;
    writeln!(stdout, "  code: 4")?;
    write!(stdout, "  file: ")?;
    write_native_path(&mut stdout, input)?;
    writeln!(stdout)?;
    writeln!(stdout, "  pos: 0")?;
    writeln!(stdout, "  text: invalid password")?;
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
    let password = password_bytes(password_arg);
    let result = Pdf::open_with_options(
        File::open(&input)?,
        PdfOpenOptions {
            password,
            suppress_password_recovery: true,
            suppress_warnings: true,
            description: path_description(&input),
            ..PdfOpenOptions::default()
        },
    );
    match result {
        Ok(mut pdf) => {
            let mut writer = PdfWriter::new(&mut pdf);
            writer.set_output_file(PathBuf::from(output_arg))?;
            writer.set_static_id(true);
            writer.write()?;
            println!("C test 2 done");
            Ok(())
        }
        Err(error) if is_bad_password(&error) => {
            report_invalid_password(&input)?;
            println!("C test 2 done");
            Ok(())
        }
        Err(error) => Err(error),
    }
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
            description: path_description(&input),
            ..PdfOpenOptions::default()
        },
    )?;

    // Match qpdf-ctest.c:test01's accessor order: version, extension level,
    // linearization, encryption, then the encrypted-only projections.
    let version = pdf.version().to_owned();
    let extension_level = pdf.adobe_extension_level()?;
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
    if let Some(extension_level) = extension_level.filter(|level| *level > 0) {
        writeln!(stdout, "extension level: {extension_level}")?;
    }
    writeln!(stdout, "linearized: {}", u8::from(linearized))?;
    writeln!(stdout, "encrypted: {}", u8::from(encrypted))?;

    if let (Some(revision), Some(user_password), Some(permissions)) =
        (encryption_revision, user_password, permissions)
    {
        write_encrypted_observations(&mut stdout, revision, &user_password, &permissions)?;
    }
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

fn write_json_test_output<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    output_arg: &std::ffi::OsStr,
    decode_level: JsonDecodeLevel,
    stream_data: JsonStreamData,
    stream_prefix: Option<&[u8]>,
    objects: &[JsonObjectSelector],
) -> Result<()> {
    let output = PathBuf::from(output_arg);
    let mut file = File::create(&output)?;
    let keys = [JsonKey::Qpdf];
    let options = JsonJobOptions {
        decode_level,
        stream_data,
        stream_prefix,
        keys: &keys,
        objects,
    };
    let mut job = QPDFJob::new();
    let _status = job
        .write_json_with_version(
            pdf,
            2,
            false,
            true,
            false,
            options,
            JsonJobOutput::File {
                filename: &output,
                writer: &mut file,
            },
        )
        .map_err(Error::from)?;
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
        JsonStreamData::Inline,
        None,
        &[],
    )?;
    println!("C test 46 done");
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
        JsonStreamData::File,
        Some(&prefix),
        &objects,
    )?;
    println!("C test 47 done");
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
    let password = password_bytes(password_arg);
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

#[cfg(test)]
mod tests {
    use super::run;
    use std::ffi::OsString;

    #[cfg(unix)]
    use super::password_bytes;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;

    #[cfg(unix)]
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
