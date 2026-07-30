use assert_cmd::Command as CargoCommand;
use std::path::Path;
use std::process::Command as ShellCommand;
use tempfile::tempdir;

const ESCAPED_KEYS: [&[u8]; 3] = [b"/Catalog#20Key", b"/Stream#20Key", b"/Trailer#20Key"];

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf is required for cli_dictionary_key_escape on CI");
    }
    eprintln!("skipping: qpdf not available");
    true
}

fn write_fixture(path: &Path) {
    let objects: [&[u8]; 4] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Catalog#20Key 7 /Payload 4 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << >> /Contents 4 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Length 0 /Stream#20Key 8 >>\nstream\nendstream\nendobj\n",
    ];
    let mut bytes = b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let startxref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 5 /Root 1 0 R /Trailer#20Key 9 >>\n\
             startxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    std::fs::write(path, bytes).expect("write fixture");
}

#[test]
fn non_qdf_dictionary_keys_match_qpdf_name_escaping() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let input = temp.path().join("input.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    write_fixture(&input);

    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--static-id"])
        .arg(&input)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_status = ShellCommand::new("qpdf")
        .arg("--static-id")
        .arg(&input)
        .arg(&qpdf_output)
        .status()
        .expect("run qpdf 11.9.0");
    assert!(qpdf_status.success(), "qpdf --static-id failed");

    let flpdf = std::fs::read(flpdf_output).expect("read flpdf output");
    let qpdf = std::fs::read(qpdf_output).expect("read qpdf output");
    for key in ESCAPED_KEYS {
        assert!(contains(&qpdf, key), "qpdf oracle omitted {key:?}");
        assert!(
            contains(&flpdf, key),
            "flpdf did not match qpdf escaping for {key:?}"
        );
    }
}
