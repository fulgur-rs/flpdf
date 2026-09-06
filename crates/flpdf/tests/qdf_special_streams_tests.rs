//! Differential coverage for qpdf's one-shot `initializeSpecialStreams` setup.

mod common;

use common::{write_with_settings, WriterTestSettings};
use flpdf::{ObjectStreamMode, Pdf};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn qpdf_available() -> bool {
    Command::new("qpdf")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn read_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    BufReader::new(File::open(path)?).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[test]
fn qdf_special_stream_fixtures_match_qpdf_static_id() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping special-stream differential");
        return Ok(());
    }

    let temporary = tempfile::tempdir()?;
    let settings = WriterTestSettings {
        qdf: true,
        static_id: true,
        object_streams: ObjectStreamMode::Disable,
        ..WriterTestSettings::default()
    };
    for name in [
        "qdf-contents-ref-array.pdf",
        "shared-page-two-parents.pdf",
        "shared-stream-objstm.pdf",
        "root-pages-points-into-tree.pdf",
    ] {
        let input = fixture(name);
        let qpdf_output = temporary.path().join(format!("qpdf-{name}"));
        let qpdf = Command::new("qpdf")
            .args(["--qdf", "--static-id", "--object-streams=disable"])
            .arg(&input)
            .arg(&qpdf_output)
            .output()
            .expect("run qpdf QDF rewrite");
        assert!(
            matches!(qpdf.status.code(), Some(0) | Some(3)),
            "qpdf QDF rewrite failed for {name}: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
        let mut actual = Vec::new();
        write_with_settings(&mut pdf, &mut actual, &settings)?;
        assert_eq!(
            actual,
            read_file(&qpdf_output)?,
            "QDF output diverges from qpdf for {name}"
        );
    }
    Ok(())
}
