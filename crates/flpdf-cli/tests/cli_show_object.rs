//! qpdf 11.9.0 `--show-object` selector and stream-output parity tests.

use assert_cmd::Command;
use std::process::Output;

#[path = "support/text.rs"]
mod text;
use text::platform_text;

const MINIMAL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/minimal.pdf"
);
const MULTI_STREAM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/multi-stream-one-page.pdf"
);
const STREAM_FLATE_ERROR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/test_driver/stream_flate_error.pdf"
);

fn flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn show_object_accepts_qpdf_selector_forms() {
    for (selector, expected) in [
        ("1", "<< /Pages 2 0 R /Type /Catalog >>\n"),
        ("1,0", "<< /Pages 2 0 R /Type /Catalog >>\n"),
        ("trailer", "<< /Root 1 0 R /Size 3 >>\n"),
    ] {
        let output = flpdf(&[&format!("--show-object={selector}"), MINIMAL]);
        assert!(output.status.success(), "{selector}: {:?}", output.stderr);
        assert_eq!(
            output.stdout,
            platform_text(expected).into_bytes(),
            "{selector}"
        );
        assert!(output.stderr.is_empty(), "{selector}: {:?}", output.stderr);
    }
}

#[test]
fn show_object_missing_selector_emits_qpdf_null() {
    let output = flpdf(&["--show-object=99,0", MINIMAL]);

    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(output.stdout, platform_text("null\n").into_bytes());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

#[test]
fn show_object_out_of_range_generation_emits_qpdf_null() {
    for selector in ["1,-1", "1,70000"] {
        let output = flpdf(&[&format!("--show-object={selector}"), MINIMAL]);

        assert!(output.status.success(), "{selector}: {:?}", output.stderr);
        assert_eq!(
            output.stdout,
            platform_text("null\n").into_bytes(),
            "{selector}"
        );
        assert!(output.stderr.is_empty(), "{selector}: {:?}", output.stderr);
    }
}

#[test]
fn show_object_keeps_qpdf_zero_object_no_output_behavior() {
    for selector in ["0", "foo", "\u{2003}1"] {
        let output = flpdf(&[&format!("--show-object={selector}"), MINIMAL]);
        assert!(output.status.success(), "{selector}: {:?}", output.stderr);
        assert!(output.stdout.is_empty(), "{selector}: {:?}", output.stdout);
        assert!(output.stderr.is_empty(), "{selector}: {:?}", output.stderr);
    }

    let generation_fallback = flpdf(&["--show-object=1,foo", MINIMAL]);
    assert!(generation_fallback.status.success());
    assert_eq!(
        generation_fallback.stdout,
        platform_text("<< /Pages 2 0 R /Type /Catalog >>\n").into_bytes()
    );
}

#[test]
fn show_object_stream_matches_qpdf_default_raw_and_filtered_modes() {
    let dictionary = flpdf(&["--show-object=4", MULTI_STREAM]);
    assert!(dictionary.status.success(), "{:?}", dictionary.stderr);
    assert_eq!(
        dictionary.stdout,
        platform_text("Object is stream.  Dictionary:\n<< /Filter /FlateDecode /Length 18 >>\n")
            .into_bytes()
    );

    let raw = flpdf(&["--show-object=4", "--raw-stream-data", MULTI_STREAM]);
    assert!(raw.status.success(), "{:?}", raw.stderr);
    assert_eq!(
        raw.stdout,
        [
            0x78, 0x9c, 0x2b, 0x54, 0x30, 0x54, 0x30, 0x00, 0x42, 0x08, 0x99, 0x9c, 0x0b, 0x00,
            0x1a, 0x69, 0x03, 0x44,
        ]
    );

    let filtered = flpdf(&["--show-object=4", "--filtered-stream-data", MULTI_STREAM]);
    assert!(filtered.status.success(), "{:?}", filtered.stderr);
    assert_eq!(filtered.stdout, b"q 1 0 0 1 0 0 cm");
}

#[test]
fn show_object_filtered_stream_failure_is_a_qpdf_warning() {
    let output = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        STREAM_FLATE_ERROR,
    ]);

    assert_eq!(output.status.code(), Some(3), "stderr: {:?}", output.stderr);
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("error decoding stream data"));
}
