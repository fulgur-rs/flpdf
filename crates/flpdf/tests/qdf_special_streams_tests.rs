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

fn indirect_crypt_filter_array_pdf() -> Vec<u8> {
    let objects = [
        (1_u32, b"<< /Type /Catalog /Pages 2 0 R /Test 6 0 R >>".to_vec()),
        (2_u32, b"<< /Type /Pages /Count 0 /Kids [ ] >>".to_vec()),
        (5_u32, b"[ /Crypt /FlateDecode ]".to_vec()),
        (
            6_u32,
            b"<< /Filter 5 0 R /DecodeParms 8 0 R /Length 11 >>\nstream\nx\x9cKLJ\x06\x00\x02M\x01'\nendstream".to_vec(),
        ),
        (8_u32, b"[ << /Name /Identity >> null ]".to_vec()),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = std::collections::BTreeMap::new();
    for (number, body) in &objects {
        offsets.insert(*number, pdf.len());
        pdf.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(b"xref\n0 9\n0000000000 65535 f \n");
    for number in 1..=8 {
        if let Some(offset) = offsets.get(&number) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        } else {
            pdf.extend_from_slice(b"0000000000 00000 f \n");
        }
    }
    pdf.extend_from_slice(b"trailer\n<< /Size 9 /Root 1 0 R >>\nstartxref\n");
    pdf.extend_from_slice(xref.to_string().as_bytes());
    pdf.extend_from_slice(b"\n%%EOF\n");
    pdf
}

fn indirect_ref_after_key(bytes: &[u8], key: &str) -> Option<u32> {
    let text = String::from_utf8_lossy(bytes);
    let marker = format!("/{key} ");
    let start = text.find(&marker)? + marker.len();
    let parts = text[start..].split_whitespace().take(3).collect::<Vec<_>>();
    (parts.len() == 3 && parts[1] == "0" && parts[2] == "R")
        .then(|| parts[0].parse().ok())
        .flatten()
}

fn object_body_contains(text: &str, number: u32, needle: &str) -> bool {
    let marker = format!("{number} 0 obj\n");
    text.find(&marker)
        .and_then(|start| text[start + marker.len()..].split_once("\nendobj\n"))
        .is_some_and(|(body, _)| body.contains(needle))
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

#[test]
fn indirect_crypt_filter_array_holder_survives_qpdf_reference_cleanup() -> flpdf::Result<()> {
    if !qpdf_available() {
        eprintln!("qpdf is unavailable; skipping indirect array-holder differential");
        return Ok(());
    }

    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("indirect-crypt-array.pdf");
    let qpdf_output = temporary.path().join("qpdf.pdf");
    std::fs::write(&input, indirect_crypt_filter_array_pdf())?;
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
        .expect("run qpdf indirect array-holder rewrite");
    assert!(
        matches!(qpdf.status.code(), Some(0) | Some(3)),
        "qpdf indirect array-holder rewrite failed: {}",
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

    let filter_holder = indirect_ref_after_key(&expected, "Filter")
        .expect("qpdf must retain an indirect /Filter holder");
    let decode_holder = indirect_ref_after_key(&expected, "DecodeParms")
        .expect("qpdf must retain an indirect /DecodeParms holder");
    let expected_text = String::from_utf8_lossy(&expected);
    assert!(
        object_body_contains(&expected_text, filter_holder, "/FlateDecode")
            && !object_body_contains(&expected_text, filter_holder, "/Crypt"),
        "qpdf must emit the Filter holder with only /FlateDecode"
    );
    assert!(
        object_body_contains(&expected_text, decode_holder, "null")
            && !object_body_contains(&expected_text, decode_holder, "/Identity"),
        "qpdf must emit the surviving DecodeParms holder"
    );
    assert_eq!(
        actual, expected,
        "indirect Filter/DecodeParms array-holder output must match qpdf"
    );
    Ok(())
}
