//! Top-level no-`--pages` page-operation transformations vs qpdf 11.9.0.
//!
//! The page-operation dispatch must carry create-stage transformations through
//! the same QPDFJob lifecycle for both ordinary and split output. This matrix
//! covers `--rotate` and `--split-pages` crossed with appearance generation
//! and annotation flattening.

#![cfg(feature = "qpdf-zlib-compat")]

use assert_cmd::Command;
use std::io::Write;
use std::path::Path;
use std::process::{Command as ProcessCommand, Output};

const EXPECTED_QPDF_VERSION: &str = "qpdf version 11.9.0";

fn qpdf_available() -> bool {
    ProcessCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .is_some_and(|line| line.trim() == EXPECTED_QPDF_VERSION)
        })
        .unwrap_or(false)
}

fn run_qpdf(args: &[String]) -> Output {
    ProcessCommand::new("qpdf")
        .args(args)
        .output()
        .expect("qpdf should spawn")
}

fn run_flpdf(args: &[String]) -> Output {
    Command::cargo_bin("flpdf")
        .expect("flpdf binary should build")
        .args(args)
        .output()
        .expect("flpdf should spawn")
}

fn assemble_pdf(objects: &[&[u8]]) -> Vec<u8> {
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for object in objects {
        offsets.push(bytes.len() as u32);
        bytes.extend_from_slice(object);
    }
    let start_xref = bytes.len();
    writeln!(&mut bytes, "xref\n0 {}", objects.len() + 1).unwrap();
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        writeln!(&mut bytes, "{offset:010} 00000 n ").unwrap();
    }
    writeln!(
        &mut bytes,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
        objects.len() + 1,
        start_xref
    )
    .unwrap();
    bytes
}

/// Two-page input with both non-no-op transformations:
/// - the generate case has a `/NeedAppearances` Tx widget with no `/AP`;
/// - the flatten case has an existing widget `/AP` and a Link `/AP`.
fn page_operation_transform_fixture(generate_case: bool) -> Vec<u8> {
    let acroform = if generate_case {
        br#"1 0 obj
<< /Type /Catalog /Pages 2 0 R
   /AcroForm << /Fields [4 0 R] /NeedAppearances true
                /DR << /Font << /Helv 10 0 R >> >>
                /DA (/Helv 12 Tf 0 g) >> >>
endobj
"#
        .to_vec()
    } else {
        br#"1 0 obj
<< /Type /Catalog /Pages 2 0 R
   /AcroForm << /Fields [4 0 R]
                /DR << /Font << /Helv 10 0 R >> >>
                /DA (/Helv 12 Tf 0 g) >> >>
endobj
"#
        .to_vec()
    };
    let widget = if generate_case {
        br#"4 0 obj
<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1)
   /V (Hello) /DA (/Helv 12 Tf 0 g)
   /Rect [100 700 300 720] /P 3 0 R >>
endobj
"#
        .to_vec()
    } else {
        br#"4 0 obj
<< /Type /Annot /Subtype /Widget /FT /Tx /T (name1)
   /V (Hello) /DA (/Helv 12 Tf 0 g)
   /Rect [100 700 300 720] /P 3 0 R /AP << /N 9 0 R >> >>
endobj
"#
        .to_vec()
    };
    let objects = vec![
        acroform,
        br#"2 0 obj
<< /Type /Pages /Count 2 /Kids [3 0 R 5 0 R] >>
endobj
"#
        .to_vec(),
        br#"3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]
   /Contents 7 0 R /Annots [4 0 R] >>
endobj
"#
        .to_vec(),
        widget,
        br#"5 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]
   /Contents 8 0 R /Annots [6 0 R] >>
endobj
"#
        .to_vec(),
        br#"6 0 obj
<< /Type /Annot /Subtype /Link /Rect [100 700 150 720]
   /Border [0 0 0] /P 5 0 R
   /A << /S /URI /URI (https://example.com) >>
   /AP << /N 9 0 R >> >>
endobj
"#
        .to_vec(),
        br#"7 0 obj
<< /Length 14 >>
stream
BT (p1) Tj ET
endstream
endobj
"#
        .to_vec(),
        br#"8 0 obj
<< /Length 14 >>
stream
BT (p2) Tj ET
endstream
endobj
"#
        .to_vec(),
        br#"9 0 obj
<< /Type /XObject /Subtype /Form /BBox [0 0 50 20] /Length 28 >>
stream
q 0 0 1 rg 0 0 50 20 re f Q
endstream
endobj
"#
        .to_vec(),
        br#"10 0 obj
<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica
   /Encoding /WinAnsiEncoding >>
endobj
"#
        .to_vec(),
    ];
    let refs = objects.iter().map(Vec::as_slice).collect::<Vec<_>>();
    assemble_pdf(&refs)
}

fn assert_process_matches(expected: &Output, actual: &Output, label: &str) {
    assert_eq!(
        actual.status.code(),
        expected.status.code(),
        "{label}: exit status differs\nqpdf stderr: {}\nflpdf stderr: {}",
        String::from_utf8_lossy(&expected.stderr),
        String::from_utf8_lossy(&actual.stderr),
    );
    assert_eq!(
        actual.stdout,
        expected.stdout,
        "{label}: stdout differs\nqpdf: {}\nflpdf: {}",
        String::from_utf8_lossy(&expected.stdout),
        String::from_utf8_lossy(&actual.stdout),
    );
    assert_eq!(
        actual.stderr,
        expected.stderr,
        "{label}: stderr differs\nqpdf: {}\nflpdf: {}",
        String::from_utf8_lossy(&expected.stderr),
        String::from_utf8_lossy(&actual.stderr),
    );
}

fn assert_pdf_outputs_match(qpdf_output: &Path, flpdf_output: &Path, split: bool, label: &str) {
    let paths = if split {
        (1..=2)
            .map(|page| {
                (
                    qpdf_output.with_file_name(format!("q-output-{page}.pdf")),
                    flpdf_output.with_file_name(format!("f-output-{page}.pdf")),
                )
            })
            .collect::<Vec<_>>()
    } else {
        vec![(qpdf_output.to_path_buf(), flpdf_output.to_path_buf())]
    };

    for (qpdf_path, flpdf_path) in paths {
        assert!(
            qpdf_path.is_file(),
            "{label}: missing qpdf output {qpdf_path:?}"
        );
        assert!(
            flpdf_path.is_file(),
            "{label}: missing flpdf output {flpdf_path:?}"
        );
        assert_eq!(
            std::fs::read(&flpdf_path).unwrap(),
            std::fs::read(&qpdf_path).unwrap(),
            "{label}: output bytes differ for qpdf={qpdf_path:?}, flpdf={flpdf_path:?}"
        );
    }
}

#[test]
fn top_level_page_operations_apply_appearance_and_flatten_transformations_like_qpdf() {
    if !qpdf_available() {
        if std::env::var_os("CI").is_some() {
            panic!("{EXPECTED_QPDF_VERSION} is required for this parity test on CI");
        }
        eprintln!("skipping: {EXPECTED_QPDF_VERSION} is not available");
        return;
    }

    let cases = [
        (
            "rotate-generate",
            "--rotate=90",
            "--generate-appearances",
            false,
        ),
        (
            "rotate-flatten",
            "--rotate=90",
            "--flatten-annotations=all",
            false,
        ),
        (
            "split-generate",
            "--split-pages=1",
            "--generate-appearances",
            true,
        ),
        (
            "split-flatten",
            "--split-pages=1",
            "--flatten-annotations=all",
            true,
        ),
    ];

    for (label, page_operation, transformation, split) in cases {
        let tempdir = tempfile::tempdir().unwrap();
        let input = tempdir.path().join("input.pdf");
        let qpdf_output = tempdir.path().join("q-output.pdf");
        let flpdf_output = tempdir.path().join("f-output.pdf");
        std::fs::write(
            &input,
            page_operation_transform_fixture(transformation == "--generate-appearances"),
        )
        .unwrap();

        let make_args = |output: &Path| {
            [
                "--qdf".to_owned(),
                "--static-id".to_owned(),
                "--no-original-object-ids".to_owned(),
                page_operation.to_owned(),
                transformation.to_owned(),
                input.display().to_string(),
                output.display().to_string(),
            ]
            .into_iter()
            .collect::<Vec<_>>()
        };
        let qpdf_args = make_args(&qpdf_output);
        let flpdf_args = make_args(&flpdf_output);
        let qpdf = run_qpdf(&qpdf_args);
        let flpdf = run_flpdf(&flpdf_args);
        assert_process_matches(&qpdf, &flpdf, label);
        assert!(qpdf.status.success(), "{label}: qpdf failed");
        assert_pdf_outputs_match(&qpdf_output, &flpdf_output, split, label);
    }
}
