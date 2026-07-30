use assert_cmd::Command;
use std::{ffi::CStr, fs};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

fn driver() -> Command {
    Command::cargo_bin("flpdf-test-driver").expect("flpdf-test-driver binary")
}

fn minimal_pdf() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/minimal.pdf"
    )
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
    driver()
        .assert()
        .code(2)
        .stdout("")
        .stderr("Usage: flpdf-test-driver n filename1 [arg2]\n");
}

#[test]
fn too_many_arguments_print_exact_usage_and_exit_two() {
    driver()
        .args(["1", minimal_pdf(), "arg2", "extra"])
        .assert()
        .code(2)
        .stdout("")
        .stderr("Usage: flpdf-test-driver n filename1 [arg2]\n");
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

    driver()
        .args(["99", malformed.to_str().expect("utf-8 temp path")])
        .assert()
        .code(2)
        .stdout("")
        .stderr("parse error at byte 0: missing PDF header\n");
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

#[cfg(unix)]
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
