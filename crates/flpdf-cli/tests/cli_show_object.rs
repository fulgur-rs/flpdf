//! qpdf 11.9.0 `--show-object` selector and stream-output parity tests.

use assert_cmd::Command;
use std::process::Output;

#[path = "support/eol.rs"]
mod eol;
use eol::EOL;

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
const STREAM_UNFILTERABLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/test_driver/stream_unfilterable.pdf"
);
const NULL_LENGTH_FRAMING: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/compat/null-length-framing-matrix.pdf"
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
        ("1", "<< /Pages 2 0 R /Type /Catalog >>"),
        ("1,0", "<< /Pages 2 0 R /Type /Catalog >>"),
        ("trailer", "<< /Root 1 0 R /Size 3 >>"),
    ] {
        let output = flpdf(&[&format!("--show-object={selector}"), MINIMAL]);
        assert!(output.status.success(), "{selector}: {:?}", output.stderr);
        assert_eq!(
            output.stdout,
            format!("{expected}{EOL}").into_bytes(),
            "{selector}"
        );
        assert!(output.stderr.is_empty(), "{selector}: {:?}", output.stderr);
    }
}

#[test]
fn show_object_missing_selector_emits_qpdf_null() {
    let output = flpdf(&["--show-object=99,0", MINIMAL]);

    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(output.stdout, format!("null{EOL}").into_bytes());
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
}

#[test]
fn show_object_out_of_range_generation_emits_qpdf_null() {
    for selector in ["1,-1", "1,70000"] {
        let output = flpdf(&[&format!("--show-object={selector}"), MINIMAL]);

        assert!(output.status.success(), "{selector}: {:?}", output.stderr);
        assert_eq!(
            output.stdout,
            format!("null{EOL}").into_bytes(),
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
        format!("<< /Pages 2 0 R /Type /Catalog >>{EOL}").into_bytes()
    );
}

#[test]
fn show_object_stream_matches_qpdf_default_raw_and_filtered_modes() {
    let dictionary = flpdf(&["--show-object=4", MULTI_STREAM]);
    assert!(dictionary.status.success(), "{:?}", dictionary.stderr);
    assert_eq!(
        dictionary.stdout,
        format!("Object is stream.  Dictionary:{EOL}<< /Filter /FlateDecode /Length 18 >>{EOL}")
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

#[test]
fn show_object_filtered_stream_applies_requested_content_normalization() {
    let output = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        "--normalize-content=y",
        NULL_LENGTH_FRAMING,
    ]);

    assert_eq!(output.status.code(), Some(3), "stderr: {:?}", output.stderr);
    assert_eq!(output.stdout, b"missing-cr\n");
}

#[test]
fn show_object_unfilterable_stream_reports_qpdf_warning_and_object_error() {
    let output = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        STREAM_UNFILTERABLE,
    ]);

    assert_eq!(output.status.code(), Some(2), "stderr: {:?}", output.stderr);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("WARNING:")
            && stderr.contains("stream object 6 0: unable to filter stream data"),
        "missing qpdf warning: {stderr}"
    );
    assert!(
        stderr.contains("unable to get object 6,0"),
        "missing qpdf object error: {stderr}"
    );
    assert!(
        !stderr.contains("getStreamData called on unfilterable stream"),
        "internal getStreamData error leaked: {stderr}"
    );
}

/// `doShowObj` reads `m->normalize`, and only `Config::normalizeContent` sets
/// it (`QPDFJob_config.cc:412-418`). QDF derives its implicit normalization
/// during writer setup instead, behind `if (m->normalize_set)`
/// (`QPDFJob.cc:2861-2863`), so `--qdf` alone must not normalize the shown
/// stream. Probed with qpdf 11.9.0 on this fixture's object 6: the bytes with
/// `--qdf` alone match the plain run, and differ from `--normalize-content=y`.
#[test]
fn show_object_filtered_stream_ignores_qdf_implicit_normalization() {
    let plain = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        NULL_LENGTH_FRAMING,
    ]);
    let with_qdf = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        "--qdf",
        NULL_LENGTH_FRAMING,
    ]);
    let normalized = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        "--normalize-content=y",
        NULL_LENGTH_FRAMING,
    ]);

    // The fixture recovers a missing `/Length`, so every run warns and exits 3
    // — qpdf does the same.
    assert_eq!(plain.status.code(), Some(3), "stderr: {:?}", plain.stderr);
    assert_eq!(with_qdf.status.code(), plain.status.code());
    assert_eq!(
        with_qdf.stdout, plain.stdout,
        "--qdf must not normalize the shown stream"
    );
    assert_ne!(
        normalized.stdout, plain.stdout,
        "the fixture must actually change under normalization, or this pins nothing"
    );
}

/// An explicit `--normalize-content=y` still normalizes when `--qdf` is also
/// given, and `=n` still suppresses it — the explicit setting is what
/// `doShowObj` reads.
#[test]
fn show_object_filtered_stream_honors_explicit_normalization_under_qdf() {
    let plain = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        NULL_LENGTH_FRAMING,
    ]);
    let qdf_yes = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        "--qdf",
        "--normalize-content=y",
        NULL_LENGTH_FRAMING,
    ]);
    let qdf_no = flpdf(&[
        "--show-object=6",
        "--filtered-stream-data",
        "--qdf",
        "--normalize-content=n",
        NULL_LENGTH_FRAMING,
    ]);

    assert_ne!(
        qdf_yes.stdout, plain.stdout,
        "an explicit =y must normalize even under --qdf"
    );
    assert_eq!(
        qdf_no.stdout, plain.stdout,
        "an explicit =n must suppress normalization under --qdf"
    );
}
