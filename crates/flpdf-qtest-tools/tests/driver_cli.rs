use assert_cmd::Command;
use std::{ffi::CStr, fs};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

fn driver() -> Command {
    Command::cargo_bin("flpdf-test-driver").expect("flpdf-test-driver binary")
}

fn usage_for(command: &Command) -> String {
    let program = command.get_program().to_string_lossy();
    let whoami = program.rsplit('/').next().unwrap_or(&program);
    format!("Usage: {whoami} n filename1 [arg2]\n")
}

fn minimal_pdf() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/minimal.pdf"
    )
}

fn fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures"))
}

fn repairable_pdf() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/test_driver/repairable_input.pdf"
    )
}

const TEST_0_OUTPUT: &str = concat!(
    "/QTest is implicit\n",
    "/QTest is direct and has type null (2)\n",
    "/QTest is null\n",
    "unparse: null\n",
    "unparseResolved: null\n",
    "test 0 done\n",
);

const TEST_1_OUTPUT: &str = concat!(
    "/QTest is implicit\n",
    "/QTest is direct and has type null (2)\n",
    "/QTest is null\n",
    "unparse: null\n",
    "unparseResolved: null\n",
    "test 1 done\n",
);

const REPAIRABLE_TEST_1_OUTPUT: &str = concat!(
    "/QTest is direct and has type boolean (3)\n",
    "/QTest is Boolean with value true\n",
    "unparse: true\n",
    "unparseResolved: true\n",
    "test 1 done\n",
);

#[test]
fn too_few_arguments_print_exact_usage_and_exit_two() {
    let mut command = driver();
    let usage = usage_for(&command);
    command.assert().code(2).stdout("").stderr(usage);
}

#[test]
fn too_many_arguments_print_exact_usage_and_exit_two() {
    let mut command = driver();
    let usage = usage_for(&command);
    command
        .args(["1", minimal_pdf(), "arg2", "extra"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(usage);
}

#[test]
fn unsupported_test_reads_valid_pdf_then_fails_loud() {
    driver()
        .args(["99", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr("invalid test 99\n");
}

#[test]
fn malformed_pdf_error_precedes_unsupported_test_lookup() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let malformed = directory.path().join("malformed.pdf");
    fs::write(&malformed, b"not a PDF").expect("write malformed fixture");
    let filename = malformed.to_str().expect("utf-8 temp path");
    let expected = format!(
        "WARNING: {filename}: can't find PDF header\n\
         WARNING: {filename}: file is damaged\n\
         WARNING: {filename}: can't find startxref\n\
         WARNING: {filename}: Attempting to reconstruct cross-reference table\n\
         {filename}: unable to find trailer dictionary while recovering damaged file\n"
    );

    driver()
        .args(["99", filename])
        .assert()
        .code(2)
        .stdout("")
        .stderr(expected);
}

#[test]
fn fourth_argument_is_accepted_but_not_used_by_id_one_family() {
    driver()
        .args(["99", minimal_pdf(), "unused"])
        .assert()
        .code(2)
        .stdout("")
        .stderr("invalid test 99\n");
}

#[test]
fn qpdf_ignore_filename_tests_do_not_open_the_dash_input() {
    driver()
        .args(["87", "-", "-"])
        .assert()
        .code(0)
        .stdout("test 87 done\n")
        .stderr("");

    driver()
        .args(["95", "-", "-"])
        .assert()
        .code(0)
        .stdout("test 95 done\n")
        .stderr("");
}

#[test]
fn object_handle_api_test_88_emits_qpdf_warning_output() {
    driver()
        .args(["88", "minimal.pdf", "-"])
        .current_dir(fixture_dir())
        .assert()
        .code(0)
        .stdout("test 88 done\n")
        .stderr(
            "WARNING: test array: ignoring attempt to erase out of bounds array item\n\
             WARNING: minimal.pdf, object 1 0 at offset 19: operation for array attempted on object of type dictionary: ignoring attempt to erase item\n",
        );
}

#[test]
fn object_handle_api_test_93_uses_canonical_promotion_route() {
    let source = include_str!("../src/driver/test_88_98.rs");

    assert!(
        source.contains("pdf.make_indirect_from_object_handle(oh1.clone())"),
        "test 93 must use qpdf-shaped in-place promotion"
    );
    assert!(
        !source.contains("GAP(QPDF::makeIndirectObject)"),
        "test 93 must not leave the qpdf promotion assertions as a GAP"
    );
}

#[test]
fn test_53_emits_all_objects_and_writes_dangling_output() {
    let directory = tempfile::tempdir().expect("temporary directory");

    driver()
        .args(["53", minimal_pdf()])
        .current_dir(directory.path())
        .assert()
        .code(0)
        .stdout(
            "new object: 3 0 R\n\
             all objects\n\
             1 0 R\n\
             2 0 R\n\
             3 0 R\n\
             test 53 done\n",
        )
        .stderr("");

    assert!(
        directory.path().join("a.pdf").is_file(),
        "test 53 must write the preserve-unreferenced output"
    );
}

#[test]
fn zero_dispatches_the_test_zero_one_family() {
    driver()
        .args(["0", minimal_pdf()])
        .assert()
        .code(0)
        .stdout(TEST_0_OUTPUT)
        .stderr("");
}

#[test]
fn one_dispatches_the_test_zero_one_family() {
    driver()
        .args(["1", minimal_pdf()])
        .assert()
        .code(0)
        .stdout(TEST_1_OUTPUT)
        .stderr("");
}

#[test]
fn zero_disables_repair_before_opening_the_input() {
    let repairable = repairable_pdf();

    driver()
        .args(["0", repairable])
        .assert()
        .code(2)
        .stdout("")
        .stderr(format!("{repairable}: can't find startxref\n"));
}

#[test]
fn one_keeps_repair_enabled_before_opening_the_input() {
    let repairable = repairable_pdf();
    let expected = format!(
        concat!(
            "WARNING: {}: file is damaged\n",
            "WARNING: {}: can't find startxref\n",
            "WARNING: {}: Attempting to reconstruct cross-reference table\n",
        ),
        repairable, repairable, repairable,
    );

    driver()
        .args(["1", repairable])
        .assert()
        .code(0)
        .stdout(REPAIRABLE_TEST_1_OUTPUT)
        .stderr(expected);
}

#[test]
fn signed_decimal_prefix_ignores_trailing_non_digits() {
    driver()
        .args([" \t+1trailing", minimal_pdf()])
        .assert()
        .code(0)
        .stdout(TEST_1_OUTPUT)
        .stderr("");
}

#[test]
fn no_digits_parse_as_zero() {
    driver()
        .args(["not-a-number", minimal_pdf()])
        .assert()
        .code(0)
        .stdout(TEST_0_OUTPUT)
        .stderr("");
}

#[test]
fn negative_decimal_prefix_reaches_unsupported_test_dispatch() {
    driver()
        .args(["-1trailing", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr("invalid test -1\n");
}

#[test]
fn i32_overflow_reports_qpdf_integer_conversion_error() {
    driver()
        .args(["2147483648", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            "integer out of range converting 2147483648 from a 8-byte signed type to a 4-byte signed type\n",
        );
}

#[test]
fn i64_overflow_reports_qpdf_decimal_conversion_error() {
    driver()
        .args(["9223372036854775808", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr("overflow/underflow converting 9223372036854775808 to 64-bit integer\n");
}

#[test]
fn u64_overflow_reports_qpdf_decimal_conversion_error() {
    driver()
        .args(["18446744073709551616", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr("overflow/underflow converting 18446744073709551616 to 64-bit integer\n");
}

#[test]
fn i64_underflow_reports_qpdf_decimal_conversion_error() {
    driver()
        .args(["-9223372036854775809", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr("overflow/underflow converting -9223372036854775809 to 64-bit integer\n");
}

#[test]
fn i64_minimum_reaches_the_qpdf_i32_range_check() {
    driver()
        .args(["-9223372036854775808", minimal_pdf()])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            "integer out of range converting -9223372036854775808 from a 8-byte signed type to a 4-byte signed type\n",
        );
}

#[test]
fn missing_input_prefixes_the_native_open_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing = directory.path().join("missing.pdf");
    let native_error = fs::read(&missing).expect_err("missing fixture must not exist");
    let error_code = native_error.raw_os_error().expect("native error code");
    let native_message = unsafe { CStr::from_ptr(libc::strerror(error_code)) }
        .to_string_lossy()
        .into_owned();
    let expected = format!("open {}: {native_message}\n", missing.display());

    driver()
        .args(["1", missing.to_str().expect("utf-8 temp path")])
        .assert()
        .code(2)
        .stdout("")
        .stderr(expected);
}

fn test_driver_fixture_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/test_driver"
    ))
}

/// Regression for the P2 review finding on test 45's open-failure path
/// (`crates/flpdf-qtest-tools/src/driver/mod.rs`): qpdf's own test 45 reads
/// `"<filename1>.obfuscated"` through `QUtil::read_file_into_memory` /
/// `QUtil::safe_fopen` (`libqpdf/QUtil.cc:1139`, `:490-518`) and only
/// fabricates the `"<filename1>.pdf"` name *after* that read succeeds, to
/// pass as the description for later parser diagnostics
/// (`test_driver.cc:3519`). An open/read failure on the real `.obfuscated`
/// path must report that real path, never the fabricated `.pdf` name.
#[test]
fn obfuscated_open_failure_reports_the_real_obfuscated_path_not_the_fabricated_pdf_name() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let base = directory.path().join("missing_obfuscated_source");
    let mut obfuscated_path = base.clone().into_os_string();
    obfuscated_path.push(".obfuscated");
    let native_error = fs::read(&obfuscated_path).expect_err(".obfuscated fixture must not exist");
    let error_code = native_error.raw_os_error().expect("native error code");
    let native_message = unsafe { CStr::from_ptr(libc::strerror(error_code)) }
        .to_string_lossy()
        .into_owned();
    let obfuscated_display = std::path::Path::new(&obfuscated_path).display().to_string();
    let expected = format!("open {obfuscated_display}: {native_message}\n");

    let assertion = driver()
        .args(["45", base.to_str().expect("utf-8 temp path")])
        .assert()
        .code(2)
        .stdout("");
    let stderr = assertion.get_output().stderr.as_slice();
    assert_eq!(stderr, expected.as_bytes());
    assert!(
        !stderr
            .windows(b".pdf".len())
            .any(|window| window == b".pdf"),
        "open failure must not mention the fabricated .pdf name: {}",
        String::from_utf8_lossy(stderr)
    );
}

/// Regression for the P2 review finding on tests 26/27/29/30's secondary
/// `arg2` open (`crates/flpdf-qtest-tools/src/driver/test_26_33.rs`): qpdf's
/// `QPDF::processFile` opens `arg2` through `FileInputSource`, which uses
/// `QUtil::safe_fopen` (`libqpdf/FileInputSource.cc:14-18`), whose failure
/// message is `"open " + path + ": " + strerror(errno)`
/// (`QPDFSystemError::createWhat`, `libqpdf/QPDFSystemError.cc:12-28`) --
/// not a bare `std::io::Error` `Display` with no operation or path.
#[test]
fn secondary_open_failure_uses_the_qpdf_open_wording_not_a_bare_io_error() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing_arg2 = directory.path().join("missing_secondary.pdf");
    let native_error = fs::read(&missing_arg2).expect_err("arg2 fixture must not exist");
    let error_code = native_error.raw_os_error().expect("native error code");
    let native_message = unsafe { CStr::from_ptr(libc::strerror(error_code)) }
        .to_string_lossy()
        .into_owned();
    let expected = format!("open {}: {native_message}\n", missing_arg2.display());

    driver()
        .args([
            "26",
            minimal_pdf(),
            missing_arg2.to_str().expect("utf-8 temp path"),
        ])
        .assert()
        .code(2)
        .stdout("")
        .stderr(expected);
}

/// Regression for the P2 review finding on test 91
/// (`crates/flpdf-qtest-tools/src/driver/test_88_98.rs`): `QPDF::writeJSON`
/// dereferences every object it serializes via `getAllObjects()`
/// (`libqpdf/QPDF_json.cc:900-925`), and a malformed object resolved lazily
/// there still goes through the ordinary `QPDF::warn` path
/// (`libqpdf/QPDF.cc:487-493`), which writes straight to the real process's
/// warn logger. `document_json::write_json` raises the same lazy-resolution
/// warning through this crate's own diagnostics collection, so it must be
/// drained after the call, or the warning silently never reaches stderr.
#[test]
fn json_write_drains_a_lazy_resolution_warning_triggered_during_serialization() {
    let assertion = driver()
        .args(["91", "dict_indirect_value_warning.pdf"])
        .current_dir(test_driver_fixture_dir())
        .assert()
        .code(0);
    let stderr = assertion.get_output().stderr.as_slice();
    assert_eq!(
        stderr,
        b"WARNING: dict_indirect_value_warning.pdf (object 7 0, offset 154): expected endobj\n"
    );
}

// macOS rejects invalid UTF-8 filenames before the driver can open them.
// The missing-path diagnostic below still exercises raw argv bytes there.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn non_utf8_pdf_path_opens_without_panicking() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut filename = b"valid-".to_vec();
    filename.push(0xff);
    filename.extend_from_slice(b".pdf");
    let path = directory
        .path()
        .join(std::ffi::OsString::from_vec(filename));
    fs::copy(minimal_pdf(), &path).expect("copy minimal PDF");

    driver()
        .arg("1")
        .arg(&path)
        .assert()
        .code(0)
        .stdout(TEST_1_OUTPUT)
        .stderr("");
}

#[cfg(unix)]
#[test]
fn missing_non_utf8_pdf_path_reports_raw_bytes_and_exit_two() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut filename = b"missing-".to_vec();
    filename.push(0xff);
    filename.extend_from_slice(b".pdf");
    let path = directory
        .path()
        .join(std::ffi::OsString::from_vec(filename.clone()));

    let assertion = driver().arg("1").arg(&path).assert().code(2).stdout("");
    let stderr = assertion.get_output().stderr.as_slice();
    assert!(stderr.starts_with(b"open "));
    assert!(stderr
        .windows(filename.len())
        .any(|window| window == filename));
    assert!(!stderr
        .windows(b"panicked at".len())
        .any(|window| window == b"panicked at"));
}
