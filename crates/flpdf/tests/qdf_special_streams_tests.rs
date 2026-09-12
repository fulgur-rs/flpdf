//! Differential coverage for qpdf's one-shot `initializeSpecialStreams` setup.

mod common;

use common::{build_pdf, write_with_settings, WriterTestSettings};
use flpdf::{ObjectStreamMode, Pdf};
use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(name)
}

fn test_driver_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/test_driver")
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

#[test]
fn normalize_content_fixtures_match_qpdf_static_id() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping normalization differential");
        return Ok(());
    }

    let temporary = tempfile::tempdir()?;
    let settings = WriterTestSettings {
        content_normalization: true,
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
        let qpdf_output = temporary.path().join(format!("qpdf-normalize-{name}"));
        let qpdf = Command::new("qpdf")
            .args([
                "--normalize-content=y",
                "--static-id",
                "--object-streams=disable",
            ])
            .arg(&input)
            .arg(&qpdf_output)
            .output()
            .expect("run qpdf normalized rewrite");
        assert!(
            matches!(qpdf.status.code(), Some(0) | Some(3)),
            "qpdf normalized rewrite failed for {name}: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );

        let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
        let mut actual = Vec::new();
        write_with_settings(&mut pdf, &mut actual, &settings)?;
        assert_eq!(
            actual,
            read_file(&qpdf_output)?,
            "normalized output diverges from qpdf for {name}"
        );
    }
    Ok(())
}

#[test]
fn qdf_stream_discovery_uses_the_prepared_dictionary() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping prepared-stream differential");
        return Ok(());
    }

    let input_bytes = build_pdf(
        &[
            (1, "<< /Pages 2 0 R /Type /Catalog >>".to_owned()),
            (2, "<< /Count 1 /Kids [3 0 R] /Type /Pages >>".to_owned()),
            (
                3,
                "<< /Contents 4 0 R /MediaBox [0 0 10 10] /Parent 2 0 R /Type /Page >>".to_owned(),
            ),
            (
                4,
                "<< /DecodeParms 6 0 R /Filter 5 0 R /Length 5 >>\nstream\n6869>\nendstream"
                    .to_owned(),
            ),
            (5, "/ASCIIHexDecode".to_owned()),
            (6, "[]".to_owned()),
        ],
        1,
    );
    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("prepared-stream-input.pdf");
    let qpdf_output = temporary.path().join("qpdf-prepared-stream.pdf");
    std::fs::write(&input, &input_bytes)?;
    let qpdf = Command::new("qpdf")
        .args(["--qdf", "--static-id", "--object-streams=disable"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf prepared-stream rewrite");
    assert!(
        qpdf.status.success(),
        "qpdf prepared-stream rewrite failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let mut pdf = Pdf::open(Cursor::new(input_bytes))?;
    let settings = WriterTestSettings {
        qdf: true,
        static_id: true,
        object_streams: ObjectStreamMode::Disable,
        ..WriterTestSettings::default()
    };
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings)?;
    assert_eq!(actual, read_file(&qpdf_output)?);
    Ok(())
}

#[test]
fn qdf_child_discovery_ignores_an_extraneous_xref_stream() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping QDF XRef exclusion differential");
        return Ok(());
    }

    let input_bytes = build_pdf(
        &[
            (
                1,
                "<< /Pages 2 0 R /Probe 4 0 R /Type /Catalog >>".to_owned(),
            ),
            (2, "<< /Count 0 /Kids [] /Type /Pages >>".to_owned()),
            (
                4,
                "<< /Length 1 /Size 5 /Type /XRef /W [1 1 1] >>\nstream\n\0\nendstream".to_owned(),
            ),
        ],
        1,
    );
    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("extraneous-xref-input.pdf");
    let qpdf_output = temporary.path().join("qpdf-extraneous-xref.pdf");
    std::fs::write(&input, &input_bytes)?;
    let qpdf = Command::new("qpdf")
        .args(["--qdf", "--static-id", "--object-streams=disable"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf extraneous-XRef rewrite");
    assert!(
        qpdf.status.success(),
        "qpdf extraneous-XRef rewrite failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let mut pdf = Pdf::open(Cursor::new(input_bytes))?;
    let settings = WriterTestSettings {
        qdf: true,
        static_id: true,
        object_streams: ObjectStreamMode::Disable,
        ..WriterTestSettings::default()
    };
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings)?;
    assert_eq!(actual, read_file(&qpdf_output)?);
    Ok(())
}

#[test]
fn qdf_uncompress_drops_indirect_decode_parms_children_before_numbering() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping indirect DecodeParms differential");
        return Ok(());
    }

    let input = test_driver_fixture("stream_decode_parms_indirect_nondict.pdf");
    let temporary = tempfile::tempdir()?;
    let qpdf_output = temporary.path().join("qpdf.pdf");
    let qpdf = Command::new("qpdf")
        .args([
            "--qdf",
            "--static-id",
            "--stream-data=uncompress",
            "--object-streams=disable",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf indirect DecodeParms rewrite");
    assert!(
        matches!(qpdf.status.code(), Some(0) | Some(3)),
        "qpdf indirect DecodeParms rewrite failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let settings = WriterTestSettings {
        qdf: true,
        static_id: true,
        object_streams: ObjectStreamMode::Disable,
        stream_data: Some(flpdf::StreamDataMode::Uncompress),
        ..WriterTestSettings::default()
    };
    let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings)?;
    let expected = read_file(&qpdf_output)?;

    assert!(
        !expected
            .windows(b"5 0 obj".len())
            .any(|window| window == b"5 0 obj"),
        "qpdf oracle must not emit the removed parameter-only child as object 5"
    );
    assert_eq!(
        actual, expected,
        "QDF uncompress numbering must not retain an indirect DecodeParms-only child"
    );
    Ok(())
}

#[test]
fn qdf_preserve_keeps_indirect_decode_parms_children_in_numbering() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping surviving indirect DecodeParms differential");
        return Ok(());
    }

    let input = test_driver_fixture("stream_decode_parms_indirect_nondict.pdf");
    let temporary = tempfile::tempdir()?;
    let qpdf_output = temporary.path().join("qpdf-preserve.pdf");
    let qpdf = Command::new("qpdf")
        .args([
            "--qdf",
            "--static-id",
            "--stream-data=preserve",
            "--object-streams=disable",
        ])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf surviving indirect DecodeParms rewrite");
    assert!(
        matches!(qpdf.status.code(), Some(0) | Some(3)),
        "qpdf surviving indirect DecodeParms rewrite failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let settings = WriterTestSettings {
        qdf: true,
        static_id: true,
        object_streams: ObjectStreamMode::Disable,
        stream_data: Some(flpdf::StreamDataMode::Preserve),
        ..WriterTestSettings::default()
    };
    let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings)?;
    let expected = read_file(&qpdf_output)?;

    assert!(
        expected
            .windows(b"/DecodeParms 5 0 R".len())
            .any(|window| window == b"/DecodeParms 5 0 R"),
        "qpdf oracle must preserve the indirect DecodeParms reference"
    );
    assert!(
        expected
            .windows(b"5 0 obj\n42".len())
            .any(|window| window == b"5 0 obj\n42"),
        "qpdf oracle must emit the surviving parameter-only child"
    );
    assert_eq!(
        actual, expected,
        "QDF preserve numbering must retain an indirect DecodeParms-only child"
    );
    Ok(())
}

#[test]
fn qdf_generate_object_stream_first_offset_matches_qpdf() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping QDF Generate differential");
        return Ok(());
    }

    let input = fixture("one-page.pdf");
    let temporary = tempfile::tempdir()?;
    let qpdf_output = temporary.path().join("qpdf-qdf-generate.pdf");
    let qpdf = Command::new("qpdf")
        .args(["--qdf", "--static-id", "--object-streams=generate"])
        .arg(&input)
        .arg(&qpdf_output)
        .output()
        .expect("run qpdf QDF Generate rewrite");
    assert!(
        matches!(qpdf.status.code(), Some(0) | Some(3)),
        "qpdf QDF Generate rewrite failed: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let settings = WriterTestSettings {
        qdf: true,
        static_id: true,
        object_streams: ObjectStreamMode::Generate,
        ..WriterTestSettings::default()
    };
    let mut pdf = Pdf::open(BufReader::new(File::open(&input)?))?;
    let mut actual = Vec::new();
    write_with_settings(&mut pdf, &mut actual, &settings)?;
    assert_eq!(
        actual,
        read_file(&qpdf_output)?,
        "QDF Generate ObjStm /First and member offsets must match qpdf"
    );
    Ok(())
}
