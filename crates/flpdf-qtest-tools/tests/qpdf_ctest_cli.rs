use assert_cmd::Command;
use flpdf::Pdf;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

fn minimal_pdf() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/minimal.pdf")
}

fn stream_pdf_without_trailing_payload_newline() -> Vec<u8> {
    let mut pdf = b"%PDF-1.3\n".to_vec();
    let mut offsets = vec![0usize];
    let mut append_object = |number: usize, body: &[u8]| {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    };

    append_object(1, b"<< /Pages 2 0 R /Type /Catalog >>");
    append_object(2, b"<< /Count 1 /Kids [3 0 R] /Type /Pages >>");
    append_object(
        3,
        b"<< /Contents 4 0 R /MediaBox [0 0 612 792] /Parent 2 0 R /Type /Page >>",
    );
    offsets.push(pdf.len());
    pdf.extend_from_slice(b"4 0 obj\n<< /Length 7 >>\nstream\npayload\nendstream\nendobj\n");

    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets.iter().skip(1) {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Root 1 0 R /Size 5 >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn encrypted_fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/encrypted")
        .join(name)
}

fn objstm_fixture() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/compat/three-page-objstm.pdf")
}

fn json_input_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/compat/json-input")
        .join(name)
}

fn create_qpdf_json_source(directory: &Path) -> PathBuf {
    let source = directory.join("json-source.pdf");
    let result = ProcessCommand::new("qpdf")
        .args(["--json-input", "--static-id"])
        .arg(json_input_fixture("complete.json"))
        .arg(&source)
        .output()
        .expect("qpdf should spawn for the JSON source fixture");
    assert!(
        result.status.success(),
        "qpdf JSON source creation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    source
}

fn run_qpdf_ctest(directory: &Path, args: &[String]) -> std::process::Output {
    let mut command = Command::cargo_bin("qpdf-ctest").expect("qpdf-ctest binary");
    command.current_dir(directory).args(args);
    command.output().expect("qpdf-ctest should spawn")
}

/// qpdf's CLI writes JSON files through a text-mode path on Windows, while
/// qpdf-ctest test46/47 explicitly use a binary `FILE*` and Rust writes LF.
/// Compare the textual JSON independent of that platform-only translation.
fn normalize_text_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;

    while let Some((&byte, rest)) = remaining.split_first() {
        if byte == b'\r' && rest.first() == Some(&b'\n') {
            normalized.push(b'\n');
            remaining = &rest[1..];
        } else {
            normalized.push(byte);
            remaining = rest;
        }
    }

    normalized
}

fn assert_json_files_equal(adapter: &Path, oracle: &Path) {
    assert_eq!(
        normalize_text_newlines(&fs::read(adapter).unwrap()),
        normalize_text_newlines(&fs::read(oracle).unwrap())
    );
}

#[test]
fn text_newline_normalization_only_collapses_crlf_pairs() {
    assert_eq!(
        normalize_text_newlines(b"first\r\nsecond\nthird\rfourth"),
        b"first\nsecond\nthird\rfourth"
    );
}

#[test]
fn qpdf_ctest_2_reports_invalid_password_through_the_c_api_error_surface() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = encrypted_fixture("v1-rc4-40-r2.pdf");
    let output = directory.path().join("unused.pdf");
    let input_name = input.to_str().expect("input path is UTF-8");
    let expected = format!(
        "error: {input_name}: invalid password\n  code: 4\n  file: {input_name}\n  pos: 0\n  text: invalid password\nC test 2 done\n"
    );

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args(["2", input_name, "wrong", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout(expected)
        .stderr("");
    assert!(
        !output.exists(),
        "test02 must not initialize a writer after auth failure"
    );
}

/// `qpdf_get_error_filename` (`qpdf-ctest.c:40`) reports the raw `argv`
/// filename qpdf was given, byte for byte, so a non-UTF-8 name must survive
/// into the "invalid password" error report unchanged rather than being
/// replaced with U+FFFD by a lossy conversion.
///
/// Linux-only: this requires actually creating a file with an
/// invalid-UTF-8 name on disk. Linux filesystems accept arbitrary
/// non-NUL/non-`/` bytes; macOS's APFS/HFS+ reject non-UTF-8 names outright
/// (`fs::copy` fails with `EILSEQ`/"Illegal byte sequence"), so this
/// scenario cannot be reproduced there.
#[test]
#[cfg(target_os = "linux")]
fn qpdf_ctest_2_reports_a_non_utf8_input_path_verbatim() {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let directory = tempfile::tempdir().expect("temporary directory");
    let source = encrypted_fixture("v1-rc4-40-r2.pdf");
    let input_name = OsString::from_vec(b"has-\xff-byte.pdf".to_vec());
    let input = directory.path().join(&input_name);
    fs::copy(&source, &input).expect("copy fixture to non-UTF-8-named path");
    let output = directory.path().join("unused.pdf");

    let mut expected = b"error: ".to_vec();
    expected.extend_from_slice(input.as_os_str().as_bytes());
    expected.extend_from_slice(b": invalid password\n  code: 4\n  file: ");
    expected.extend_from_slice(input.as_os_str().as_bytes());
    expected.extend_from_slice(b"\n  pos: 0\n  text: invalid password\nC test 2 done\n");

    let assert = Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .arg("2")
        .arg(&input)
        .arg("wrong")
        .arg(&output)
        .assert()
        .success()
        .stderr("");
    assert_eq!(assert.get_output().stdout, expected);
}

#[test]
fn qpdf_ctest_2_writes_output_and_completes_on_successful_authentication() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = minimal_pdf();
    let output = directory.path().join("test-2.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args(["2", input.to_str().unwrap(), "", output.to_str().unwrap()])
        .assert()
        .success()
        .stdout("C test 2 done\n")
        .stderr("");

    let pdf = Pdf::open(std::fs::File::open(&output).expect("open ctest output"))
        .expect("test02's written output must be a valid PDF");
    assert!(!pdf.is_encrypted());
}

#[test]
fn qpdf_ctest_encryption_writer_cases_cover_r2_through_r6() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = minimal_pdf();
    let cases = [
        ("11", "user1"),
        ("12", "user2"),
        ("15", "user2"),
        ("17", "user3"),
        ("18", "user4"),
    ];

    for (test_number, user_password) in cases {
        let output = directory.path().join(format!("test-{test_number}.pdf"));
        Command::cargo_bin("qpdf-ctest")
            .expect("qpdf-ctest binary")
            .args([
                test_number,
                input.to_str().unwrap(),
                "",
                output.to_str().unwrap(),
            ])
            .assert()
            .success()
            .stdout(format!("C test {test_number} done\n"))
            .stderr("");

        let pdf = Pdf::open_with_options(
            std::fs::File::open(&output).expect("open ctest output"),
            flpdf::PdfOpenOptions {
                password: user_password.as_bytes().to_vec(),
                ..flpdf::PdfOpenOptions::default()
            },
        )
        .expect("ctest encryption output must authenticate");
        assert!(pdf.is_encrypted());
    }
}

#[test]
fn qpdf_ctest_preserves_qpdf_objstm_enqueue_order_for_encryption_and_decryption() {
    let qpdf_available = ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !qpdf_available {
        eprintln!("qpdf is not available; skipping qpdf ObjStm enqueue-order oracle test");
        return;
    }

    let directory = tempfile::tempdir().expect("temporary directory");
    let input = objstm_fixture();
    let qpdf_encrypted = directory.path().join("qpdf-r2.pdf");
    let flpdf_encrypted = directory.path().join("flpdf-r2.pdf");
    let qpdf_decrypted = directory.path().join("qpdf-decrypted.pdf");
    let flpdf_decrypted = directory.path().join("flpdf-decrypted.pdf");

    let qpdf_encrypt = ProcessCommand::new("qpdf")
        .args([
            "--static-id",
            "--allow-weak-crypto",
            "--object-streams=preserve",
            "--encrypt",
            "user1",
            "owner1",
            "40",
            "--print=n",
            "--modify=y",
            "--extract=y",
            "--annotate=y",
            "--",
        ])
        .arg(&input)
        .arg(&qpdf_encrypted)
        .status()
        .expect("run qpdf R2 encryption oracle");
    assert!(qpdf_encrypt.success(), "qpdf R2 encryption oracle failed");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "11",
            input.to_str().expect("fixture path is UTF-8"),
            "",
            flpdf_encrypted.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("C test 11 done\n")
        .stderr("");

    let flpdf_difference = flpdf_qtest_tools::compare_files(
        &fs::read(&flpdf_encrypted).expect("read flpdf encrypted output"),
        &fs::read(&qpdf_encrypted).expect("read qpdf encrypted output"),
        b"user1",
    )
    .expect("compare encrypted outputs");
    assert_eq!(
        flpdf_difference, None,
        "encrypted C API output must use qpdf source-backed ObjStm numbering"
    );

    let qpdf_decrypt = ProcessCommand::new("qpdf")
        .args(["--static-id", "--password=user1", "--decrypt"])
        .arg(&qpdf_encrypted)
        .arg(&qpdf_decrypted)
        .status()
        .expect("run qpdf decryption oracle");
    assert!(qpdf_decrypt.success(), "qpdf decryption oracle failed");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "13",
            qpdf_encrypted.to_str().expect("encrypted path is UTF-8"),
            "user1",
            flpdf_decrypted.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("user password: user1\nC test 13 done\n")
        .stderr("");

    let flpdf_decrypted_difference = flpdf_qtest_tools::compare_files(
        &fs::read(&flpdf_decrypted).expect("read flpdf decrypted output"),
        &fs::read(&qpdf_decrypted).expect("read qpdf decrypted output"),
        b"",
    )
    .expect("compare decrypted outputs");
    assert_eq!(
        flpdf_decrypted_difference, None,
        "decrypted C API output must preserve qpdf source-backed ObjStm numbering"
    );
}

#[test]
fn qpdf_ctest_13_recovers_the_user_password_and_decrypts() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = encrypted_fixture("v2-rc4-128-r3.pdf");
    let output = directory.path().join("decrypted.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "13",
            input.to_str().unwrap(),
            "user-v2",
            output.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout("user password: user-v2\nC test 13 done\n")
        .stderr("");

    let pdf = Pdf::open(std::io::Cursor::new(std::fs::read(output).unwrap()))
        .expect("test13 output must be plaintext");
    assert!(!pdf.is_encrypted());
}

#[test]
fn qpdf_ctest_19_writes_the_same_deterministic_pdf_twice() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("input.pdf");
    let first = directory.path().join("first.pdf");
    let second = directory.path().join("second.pdf");
    fs::copy(minimal_pdf(), &input).expect("copy input PDF");

    for output in [&first, &second] {
        Command::cargo_bin("qpdf-ctest")
            .expect("qpdf-ctest binary")
            .args([
                "19",
                input.to_str().expect("input path is UTF-8"),
                "",
                output.to_str().expect("output path is UTF-8"),
            ])
            .assert()
            .success()
            .stdout("C test 19 done\n")
            .stderr("");
    }

    assert_eq!(
        fs::read(first).expect("read first output"),
        fs::read(second).expect("read second output"),
        "qpdf-ctest test19 must preserve qpdf deterministic-ID repeatability"
    );
}

#[test]
fn qpdf_ctest_version_reports_the_pinned_qpdf_version() {
    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .arg("--version")
        .assert()
        .success()
        .stdout("qpdf-ctest version 11.9.0\n")
        .stderr("");
}

#[test]
fn qpdf_ctest_json_cases_42_through_47_match_qpdf() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let complete = json_input_fixture("complete.json");
    let update = json_input_fixture("update.json");
    let complete_str = complete.to_str().expect("fixture path is UTF-8");
    let update_str = update.to_str().expect("fixture path is UTF-8");

    let output42 = directory.path().join("ctest-42.pdf");
    let result42 = run_qpdf_ctest(
        directory.path(),
        &[
            "42".to_owned(),
            complete_str.to_owned(),
            String::new(),
            output42.to_str().unwrap().to_owned(),
        ],
    );
    assert!(
        result42.status.success(),
        "qpdf-ctest 42 failed: {}",
        String::from_utf8_lossy(&result42.stderr)
    );
    assert_eq!(result42.stdout, b"C test 42 done\n");
    assert!(result42.stderr.is_empty());

    let output43 = directory.path().join("ctest-43.pdf");
    let result43 = run_qpdf_ctest(
        directory.path(),
        &[
            "43".to_owned(),
            complete_str.to_owned(),
            String::new(),
            output43.to_str().unwrap().to_owned(),
        ],
    );
    assert!(
        result43.status.success(),
        "qpdf-ctest 43 failed: {}",
        String::from_utf8_lossy(&result43.stderr)
    );
    assert_eq!(result43.stdout, b"C test 43 done\n");
    assert!(result43.stderr.is_empty());

    let qpdf_create = directory.path().join("qpdf-create.pdf");
    let qpdf_create_result = ProcessCommand::new("qpdf")
        .args(["--json-input", "--static-id"])
        .arg(&complete)
        .arg(&qpdf_create)
        .output()
        .expect("qpdf should spawn for test42 oracle");
    assert!(qpdf_create_result.status.success());
    assert_eq!(
        fs::read(&output42).unwrap(),
        fs::read(&qpdf_create).unwrap()
    );
    assert_eq!(
        fs::read(&output43).unwrap(),
        fs::read(&qpdf_create).unwrap()
    );

    let source = create_qpdf_json_source(directory.path());
    let source_str = source.to_str().expect("source path is UTF-8");
    let output44 = directory.path().join("ctest-44.pdf");
    let result44 = run_qpdf_ctest(
        directory.path(),
        &[
            "44".to_owned(),
            source_str.to_owned(),
            String::new(),
            output44.to_str().unwrap().to_owned(),
            update_str.to_owned(),
        ],
    );
    assert!(
        result44.status.success(),
        "qpdf-ctest 44 failed: {}",
        String::from_utf8_lossy(&result44.stderr)
    );
    assert_eq!(result44.stdout, b"C test 44 done\n");
    assert!(result44.stderr.is_empty());

    let output45 = directory.path().join("ctest-45.pdf");
    let result45 = run_qpdf_ctest(
        directory.path(),
        &[
            "45".to_owned(),
            source_str.to_owned(),
            String::new(),
            output45.to_str().unwrap().to_owned(),
            update_str.to_owned(),
        ],
    );
    assert!(
        result45.status.success(),
        "qpdf-ctest 45 failed: {}",
        String::from_utf8_lossy(&result45.stderr)
    );
    assert_eq!(result45.stdout, b"C test 45 done\n");
    assert!(result45.stderr.is_empty());

    let qpdf_update = directory.path().join("qpdf-update.pdf");
    let qpdf_update_result = ProcessCommand::new("qpdf")
        .arg("--static-id")
        .arg(format!("--update-from-json={update_str}"))
        .arg(&source)
        .arg(&qpdf_update)
        .output()
        .expect("qpdf should spawn for test44 oracle");
    assert!(qpdf_update_result.status.success());
    assert_eq!(
        fs::read(&output44).unwrap(),
        fs::read(&qpdf_update).unwrap()
    );
    assert_eq!(
        fs::read(&output45).unwrap(),
        fs::read(&qpdf_update).unwrap()
    );

    let output46 = directory.path().join("ctest-46.json");
    let result46 = run_qpdf_ctest(
        directory.path(),
        &[
            "46".to_owned(),
            source_str.to_owned(),
            String::new(),
            output46.to_str().unwrap().to_owned(),
        ],
    );
    assert!(
        result46.status.success(),
        "qpdf-ctest 46 failed: {}",
        String::from_utf8_lossy(&result46.stderr)
    );
    assert_eq!(result46.stdout, b"C test 46 done\n");
    assert!(result46.stderr.is_empty());

    let qpdf_json46 = directory.path().join("qpdf-46.json");
    let qpdf_json46_result = ProcessCommand::new("qpdf")
        .args([
            "--json-output=2",
            "--json-stream-data=inline",
            "--decode-level=none",
        ])
        .arg(&source)
        .arg(&qpdf_json46)
        .output()
        .expect("qpdf should spawn for test46 oracle");
    assert!(qpdf_json46_result.status.success());
    assert_json_files_equal(&output46, &qpdf_json46);

    let adapter47 = directory.path().join("adapter-47");
    let oracle47 = directory.path().join("oracle-47");
    fs::create_dir(&adapter47).unwrap();
    fs::create_dir(&oracle47).unwrap();
    let output47 = adapter47.join("ctest-47.json");
    let result47 = run_qpdf_ctest(
        &adapter47,
        &[
            "47".to_owned(),
            source_str.to_owned(),
            String::new(),
            output47.to_str().unwrap().to_owned(),
            "auto".to_owned(),
        ],
    );
    assert!(
        result47.status.success(),
        "qpdf-ctest 47 failed: {}",
        String::from_utf8_lossy(&result47.stderr)
    );
    assert_eq!(result47.stdout, b"C test 47 done\n");
    assert!(result47.stderr.is_empty());

    let qpdf_json47 = oracle47.join("qpdf-47.json");
    let qpdf_json47_result = ProcessCommand::new("qpdf")
        .current_dir(&oracle47)
        .args([
            "--json-output=2",
            "--json-stream-data=file",
            "--json-stream-prefix=auto",
            "--decode-level=specialized",
            "--json-object=4",
            "--json-object=trailer",
        ])
        .arg(&source)
        .arg(&qpdf_json47)
        .output()
        .expect("qpdf should spawn for test47 oracle");
    assert!(qpdf_json47_result.status.success());
    assert_json_files_equal(&output47, &qpdf_json47);
    assert_eq!(
        fs::read(adapter47.join("auto-4")).unwrap(),
        fs::read(oracle47.join("auto-4")).unwrap()
    );
}

#[test]
fn qpdf_ctest_1_reports_plaintext_metadata_and_ignores_outfile() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("unused-output.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "1",
            minimal_pdf().to_str().expect("input path is UTF-8"),
            "",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("version: 1.7\nlinearized: 0\nencrypted: 0\nC test 1 done\n")
        .stderr("");

    assert!(
        !output.exists(),
        "test01 must not write its outfile argument"
    );
}

#[test]
fn qpdf_ctest_1_reports_linearized_metadata() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("unused-output.pdf");
    let input = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/compat/linearized-one-page.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "1",
            input.to_str().expect("input path is UTF-8"),
            "",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("version: 1.3\nlinearized: 1\nencrypted: 0\nC test 1 done\n")
        .stderr("");

    assert!(
        !output.exists(),
        "test01 must not write its outfile argument"
    );
}

#[test]
fn qpdf_ctest_1_reports_encryption_metadata_and_permissions() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("unused-output.pdf");
    let input = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/encrypted/v2-rc4-128-r3.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "1",
            input.to_str().expect("input path is UTF-8"),
            "user-v2",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout(concat!(
            "version: 1.7\n",
            "linearized: 0\n",
            "encrypted: 1\n",
            "user password: user-v2\n",
            "extract for accessibility: 1\n",
            "extract for any purpose: 1\n",
            "print low resolution: 1\n",
            "print high resolution: 1\n",
            "modify document assembly: 1\n",
            "modify forms: 1\n",
            "modify annotations: 1\n",
            "modify other: 1\n",
            "modify anything: 1\n",
            "C test 1 done\n",
        ))
        .stderr("");

    assert!(
        !output.exists(),
        "test01 must not write its outfile argument"
    );
}

#[test]
fn qpdf_ctest_20_writes_with_specialized_decode_level() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let output = directory.path().join("output.pdf");
    let input = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/test_driver/stream_dct.pdf");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "20",
            input.to_str().expect("input path is UTF-8"),
            "",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("C test 20 done\n")
        .stderr("");

    assert!(output.is_file(), "test20 must write its output PDF");
    let mut pdf = Pdf::open(Cursor::new(fs::read(&output).expect("read output PDF")))
        .expect("open test20 output PDF");
    let has_dct_stream = pdf.object_refs().into_iter().any(|object_ref| {
        let object = pdf.get_object_handle(object_ref);
        pdf.resolve(&object).is_ok()
            && object
                .as_stream_dict()
                .and_then(|dict| dict.get_key(b"/Filter").as_name())
                .is_some_and(|name| name == b"DCTDecode")
    });
    assert!(
        has_dct_stream,
        "specialized decode level must preserve the lossy DCT filter"
    );
}

#[test]
fn qpdf_ctest_22_writes_newline_before_endstream() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("input.pdf");
    let output = directory.path().join("output.pdf");
    fs::write(&input, stream_pdf_without_trailing_payload_newline())
        .expect("write test22 input PDF");

    Command::cargo_bin("qpdf-ctest")
        .expect("qpdf-ctest binary")
        .args([
            "22",
            input.to_str().expect("input path is UTF-8"),
            "",
            output.to_str().expect("output path is UTF-8"),
        ])
        .assert()
        .success()
        .stdout("C test 22 done\n")
        .stderr("");

    let output = fs::read(output).expect("read test22 output PDF");
    assert!(
        output
            .windows(b"payload\nendstream".len())
            .any(|window| { window == b"payload\nendstream" }),
        "test22 must insert a newline between the payload and endstream"
    );
}
