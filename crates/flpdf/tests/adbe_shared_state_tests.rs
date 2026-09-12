//! qpdf QPDFWriter.cc:1347-1435 shares an existing direct Extensions dictionary.
//! See tests/oracle/qpdf_adbe_shared_state_probe.cc for independent state evidence.
use flpdf::{ObjectHandle, Pdf, PdfWriter};
use std::io::{self, Cursor, Write};

struct FailingOutput;
impl Write for FailingOutput {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("output failure"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn document() -> (Pdf<Cursor<Vec<u8>>>, ObjectHandle, ObjectHandle) {
    let mut pdf = Pdf::open(Cursor::new(
        include_bytes!("../../../tests/fixtures/compat/one-page.pdf").to_vec(),
    ))
    .unwrap();
    let root = pdf.root_handle().unwrap();
    let extensions = ObjectHandle::dictionary(vec![
        (
            b"/ADBE".to_vec(),
            ObjectHandle::dictionary(vec![
                (
                    b"/BaseVersion".to_vec(),
                    ObjectHandle::name(b"/1.7".to_vec()),
                ),
                (b"/ExtensionLevel".to_vec(), ObjectHandle::integer(3)),
            ]),
        ),
        (b"/ACME".to_vec(), ObjectHandle::integer(1)),
    ]);
    root.replace_key(b"/Extensions", extensions.clone())
        .unwrap();
    (pdf, root, extensions)
}

fn shared_extensions(linearize: bool, level: i64, fail: bool) {
    let (mut pdf, root, extensions) = document();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_extra_header_text("% ADBE probe\n"); // selects specialized output
    writer.set_static_id(true);
    writer.force_pdf_version("1.7", level);
    writer.set_linearization(linearize);
    if fail {
        writer.set_output_writer(FailingOutput).unwrap();
    } else {
        writer.set_output_memory().unwrap();
    }
    assert_eq!(writer.write().is_err(), fail);
    assert!(root
        .try_get_key(b"/Extensions")
        .unwrap()
        .is_same_object_as(&extensions));
    assert_eq!(
        extensions
            .try_get_key(b"/ACME")
            .unwrap()
            .try_get_int_value()
            .unwrap(),
        1
    );
    let adbe = extensions.try_get_key(b"/ADBE").unwrap();
    if level == 0 {
        assert!(
            adbe.is_null(),
            "shared ADBE removal must survive write completion"
        );
    } else {
        assert_eq!(
            adbe.try_get_key(b"/ExtensionLevel")
                .unwrap()
                .try_get_int_value()
                .unwrap(),
            level
        );
    }
}

#[test]
fn linearized_adbe_changes_the_existing_shared_dictionary() {
    for level in [0, 8] {
        shared_extensions(true, level, false);
    }
}
#[test]
fn linearized_sink_failure_keeps_shared_adbe_changes() {
    for level in [0, 8] {
        shared_extensions(true, level, true);
    }
}

fn callback_failure(percent: u8, expected_level: i64) {
    let (mut pdf, _root, extensions) = document();
    let mut writer = PdfWriter::new(&mut pdf);
    writer.set_linearization(true);
    writer.force_pdf_version("1.7", 8);
    writer.set_output_memory().unwrap();
    writer.register_progress_reporter(Box::new(move |current| {
        if current == percent {
            return Err(flpdf::Error::System("callback failure".into()));
        }
        Ok(())
    }));
    let error = writer.write().unwrap_err();
    assert!(matches!(error, flpdf::Error::System(ref message) if message == "callback failure"));
    assert_eq!(
        extensions
            .try_get_key(b"/ADBE")
            .unwrap()
            .try_get_key(b"/ExtensionLevel")
            .unwrap()
            .try_get_int_value()
            .unwrap(),
        expected_level
    );
}

#[test]
fn linearized_first_root_callback_failure_does_not_reconcile_extensions() {
    callback_failure(0, 3);
}

#[test]
fn linearized_second_pass_callback_failure_keeps_first_pass_changes() {
    // The fixture has seven body objects. qpdf reports 0..51 in pass 1;
    // 58 is the Catalog callback in pass 2 (the oracle's trace mode).
    callback_failure(58, 8);
}

#[cfg(feature = "qpdf-zlib-compat")]
#[test]
fn linearized_extension_bytes_match_qpdf_with_and_without_encryption() {
    use std::{fs, path::Path, process::Command};
    let version = Command::new("qpdf").arg("--version").output();
    if !version.is_ok_and(|result| {
        result.status.success() && result.stdout.starts_with(b"qpdf version 11.9.0\n")
    }) {
        eprintln!("qpdf 11.9.0 unavailable; differential not executed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    for fixture in [
        "one-page.pdf",
        "linearize-indirect-extensions.pdf",
        "one-page-stale-adbe-no-ext-vendor.pdf",
        "one-page-stale-adbe-no-ext.pdf",
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/compat")
            .join(fixture);
        for encrypt in [false, true] {
            for level in [0, 8] {
                let mut pdf = Pdf::open(Cursor::new(fs::read(&input).unwrap())).unwrap();
                let mut writer = PdfWriter::new(&mut pdf);
                writer.set_linearization(true);
                writer.set_static_id(true);
                writer.set_static_aes_iv(true);
                writer.force_pdf_version("1.7", level);
                if encrypt {
                    writer.set_encryption_parameters(flpdf::EncryptParams::v4_aes128(
                        b"u".to_vec(),
                        b"o".to_vec(),
                    ));
                }
                writer.set_output_memory().unwrap();
                writer.write().unwrap();
                let actual = writer.get_buffer().unwrap();
                let oracle_path = temp.path().join("oracle.pdf");
                let mut command = Command::new("qpdf");
                command
                    .args(["--linearize", "--static-id", "--static-aes-iv"])
                    .arg(format!("--force-version=1.7.{level}"));
                if encrypt {
                    command.args(["--encrypt", "u", "o", "128", "--use-aes=y", "--"]);
                }
                let oracle = command.arg(&input).arg(&oracle_path).output().unwrap();
                assert!(oracle.status.success(), "{fixture}: {:?}", oracle.stderr);
                assert_eq!(
                    actual,
                    fs::read(&oracle_path).unwrap(),
                    "{fixture} encrypt={encrypt} level={level}"
                );
                let actual_path = temp.path().join("actual.pdf");
                fs::write(&actual_path, actual).unwrap();
                let check = Command::new("qpdf")
                    .args(["--password=u", "--check-linearization"])
                    .arg(actual_path)
                    .output()
                    .unwrap();
                assert!(check.status.success(), "{fixture}: {:?}", check.stderr);
            }
        }
    }
}
