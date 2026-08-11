use assert_cmd::Command;
use std::fs;

fn fixture_path(name: &str) -> String {
    format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn test_xref_formats_the_effective_source_table_in_object_order() {
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(fixture_path("minimal.pdf"))
        .assert()
        .success()
        .stdout(
            "1/0, uncompressed, offset = 9 (0x9)\n\
             2/0, uncompressed, offset = 58 (0x3a)\n",
        )
        .stderr("");
}

#[test]
fn test_xref_preserves_compressed_stream_number_and_index() {
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(fixture_path("compat/three-page-objstm.pdf"))
        .assert()
        .success()
        .stdout(concat!(
            "1/0, uncompressed, offset = 15 (0xf)\n",
            "2/0, compressed, stream number = 1, stream index = 0\n",
            "3/0, compressed, stream number = 1, stream index = 1\n",
            "4/0, compressed, stream number = 1, stream index = 2\n",
            "5/0, compressed, stream number = 1, stream index = 3\n",
            "6/0, compressed, stream number = 1, stream index = 4\n",
            "7/0, compressed, stream number = 1, stream index = 5\n",
            "8/0, compressed, stream number = 1, stream index = 6\n",
            "9/0, compressed, stream number = 1, stream index = 7\n",
            "10/0, uncompressed, offset = 532 (0x214)\n",
            "11/0, uncompressed, offset = 685 (0x2ad)\n",
            "12/0, uncompressed, offset = 838 (0x346)\n",
            "13/0, uncompressed, offset = 991 (0x3df)\n",
        ))
        .stderr("");
}

#[test]
fn test_parsedoffset_walks_direct_children_and_formats_qpdf_offsets() {
    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(fixture_path("minimal.pdf"))
        .assert()
        .success()
        .stdout(
            "--- objects not in streams ---\n\
             offset = 17 (0x11), indirect 1/0, dictionary\n\
             offset = 26 (0x1a), direct, name\n\
             offset = 66 (0x42), indirect 2/0, dictionary\n\
             offset = 75 (0x4b), direct, name\n\
             offset = 89 (0x59), direct, integer\n\
             offset = 97 (0x61), direct, array\n\
             succeeded\n",
        )
        .stderr("");
}

#[test]
fn test_parsedoffset_groups_objects_in_object_streams() {
    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(fixture_path("compat/three-page-objstm.pdf"))
        .assert()
        .success()
        .stdout(predicates::str::contains("--- objects in stream 1 ---"))
        .stdout(predicates::str::contains(
            "offset = 45 (0x2d), indirect 2/0, dictionary",
        ))
        .stdout(predicates::str::contains(
            "offset = 61 (0x3d), indirect 3/0, dictionary",
        ))
        .stdout(predicates::str::contains("succeeded\n"))
        .stderr("");
}

#[test]
fn metadata_helpers_match_qpdf_usage_contracts() {
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .assert()
        .code(2)
        .stdout("")
        .stderr("usage: test_xref INPUT.pdf\n");

    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .assert()
        .code(2)
        .stdout("")
        .stderr("Usage: test_parsedoffset INPUT.pdf\n");
}

#[test]
fn test_xref_reports_missing_input_with_qpdf_open_wording() {
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg("/definitely/missing/flpdf-metadata.pdf")
        .assert()
        .code(2)
        .stdout("")
        .stderr("open /definitely/missing/flpdf-metadata.pdf: No such file or directory\n");
}

#[test]
fn test_xref_preserves_qpdf_recovery_diagnostics_before_terminal_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let input = directory.path().join("malformed.pdf");
    fs::write(&input, b"garbage").expect("write malformed input");
    let path = input.display();
    let expected = format!(
        "WARNING: {path}: can't find PDF header\n\
         WARNING: {path}: file is damaged\n\
         WARNING: {path}: can't find startxref\n\
         WARNING: {path}: Attempting to reconstruct cross-reference table\n\
         {path}: unable to find trailer dictionary while recovering damaged file\n"
    );

    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(&input)
        .assert()
        .code(2)
        .stdout("")
        .stderr(expected);
}

#[test]
fn test_parsedoffset_rejects_an_enumerated_object_missing_from_xref() {
    Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(fixture_path("compat/dangling-body-one-page.pdf"))
        .assert()
        .code(2)
        .stdout("")
        .stderr("99/0 is not found in xref table\n");
}

#[test]
fn test_parsedoffset_preserves_repair_warnings_before_post_enumeration_failure() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut input_bytes =
        fs::read(fixture_path("test_driver/repairable_input.pdf")).expect("read repairable input");
    let marker = b"/QTest true >>";
    let marker_start = input_bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("repairable trailer marker");
    let insert_at = marker_start + b"/QTest true".len();
    input_bytes.splice(insert_at..insert_at, b" /Info 99 0 R".iter().copied());

    let input = directory.path().join("repairable-with-dangling-info.pdf");
    fs::write(&input, input_bytes).expect("write repaired-input variant");
    let path = input.display();

    let output = Command::cargo_bin("test_parsedoffset")
        .expect("test_parsedoffset binary")
        .arg(&input)
        .output()
        .expect("run test_parsedoffset");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());

    let stderr = String::from_utf8_lossy(&output.stderr);
    let warning_start = format!(
        "WARNING: {path}: file is damaged\n\
         WARNING: {path}: can't find startxref\n\
         WARNING: {path}: Attempting to reconstruct cross-reference table\n"
    );
    assert!(
        stderr.starts_with(&warning_start),
        "repair warnings must precede the terminal error: {stderr}"
    );
    assert!(
        stderr.ends_with(&format!("{path}: 99/0 is not found in xref table\n")),
        "the post-enumeration error must remain visible: {stderr}"
    );
}

#[test]
fn test_xref_matches_qpdf_recovered_xref_and_warning_order() {
    let path = fixture_path("test_driver/repairable_input.pdf");
    Command::cargo_bin("test_xref")
        .expect("test_xref binary")
        .arg(path)
        .assert()
        .success()
        .stdout(concat!(
            "1/0, uncompressed, offset = 9 (0x9)\n",
            "2/0, uncompressed, offset = 58 (0x3a)\n",
        ))
        .stderr(concat!(
            "WARNING: ",
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/repairable_input.pdf: file is damaged\n",
            "WARNING: ",
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/repairable_input.pdf: can't find startxref\n",
            "WARNING: ",
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/test_driver/repairable_input.pdf: Attempting to reconstruct cross-reference table\n",
        ));
}
