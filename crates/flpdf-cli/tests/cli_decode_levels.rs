//! qpdf-compatible `--decode-level` writer coverage.

use assert_cmd::Command;
use flpdf::{Object, Pdf};
use std::io::Cursor;

mod common;
use common::PdfCanonicalTestExt;

const FIXTURE: &str = "../../tests/fixtures/test_driver/stream_dct.pdf";

fn dct_stream_count(path: &std::path::Path) -> usize {
    let bytes = std::fs::read(path).expect("read rewritten PDF");
    let mut pdf = Pdf::open(Cursor::new(bytes)).expect("open rewritten PDF");
    pdf.object_refs()
        .into_iter()
        .filter_map(|object_ref| pdf.resolve_canonical_object(object_ref).ok())
        .filter(|object| {
            matches!(
                object,
                Object::Stream(stream)
                    if matches!(
                        stream.dict.get("Filter"),
                        Some(Object::Name(name)) if name == b"DCTDecode"
                    )
            )
        })
        .count()
}

#[test]
fn top_level_decode_level_controls_lossy_dct_filtering() {
    for (level, expected_dct_streams) in [
        ("none", 1),
        ("generalized", 1),
        ("specialized", 1),
        ("all", 0),
    ] {
        let temp = tempfile::tempdir().expect("temporary directory");
        let output = temp.path().join(format!("{level}.pdf"));

        Command::cargo_bin("flpdf")
            .expect("flpdf binary")
            .args([
                "--compress-streams=n",
                &format!("--decode-level={level}"),
                "--static-id",
                FIXTURE,
                output.to_str().expect("output path is UTF-8"),
            ])
            .assert()
            .success();

        assert!(output.is_file(), "decode level {level} must create output");
        assert_eq!(
            dct_stream_count(&output),
            expected_dct_streams,
            "decode level {level} must preserve or decode the DCT stream"
        );
    }
}
