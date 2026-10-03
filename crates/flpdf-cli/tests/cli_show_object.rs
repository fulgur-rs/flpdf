//! qpdf 11.9.0 `--show-object` selector and stream-output parity tests.

use assert_cmd::Command;
use std::process::Command as ShellCommand;
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
const DCT_TWO_COMPONENT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/dct-two-component.pdf"
);
const DCT_RESERVED_MARKER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/dct-reserved-marker.pdf"
);

fn flpdf(args: &[&str]) -> Output {
    Command::cargo_bin("flpdf")
        .unwrap()
        .args(args)
        .output()
        .unwrap()
}

fn tiff_predictor_wrapped_geometry_pdf() -> Vec<u8> {
    const FLATE_ABCD: &[u8] = &[
        0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0x4e, 0x01, 0x00, 0x03, 0xd8, 0x01, 0x8b,
    ];

    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (object_number, body) in [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Count 0 /Kids [] >>".to_vec()),
        (
            3,
            [
                format!(
                    "<< /Length {} /Filter /FlateDecode /DecodeParms << /Predictor 2 /Colors 4 /BitsPerComponent 8 /Columns 1073741825 >> >>\nstream\n",
                    FLATE_ABCD.len()
                )
                .as_bytes(),
                FLATE_ABCD,
                b"\nendstream",
            ]
            .concat(),
        ),
    ] {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{object_number} 0 obj\n").as_bytes());
        bytes.extend_from_slice(&body);
        bytes.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes(),
    );
    bytes
}

fn qpdf_11_9_available() -> bool {
    let Ok(output) = ShellCommand::new("qpdf").arg("--version").output() else {
        return false;
    };
    let version = String::from_utf8_lossy(&output.stdout);
    output.status.success() && version.lines().next().map(str::trim) == Some("qpdf version 11.9.0")
}

fn normalize_diagnostic_program_name(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .replace(
            "\nqpdf: operation succeeded with warnings",
            "\nTOOL: operation succeeded with warnings",
        )
        .replace(
            "\nflpdf: operation succeeded with warnings",
            "\nTOOL: operation succeeded with warnings",
        )
}

fn two_component_fractional_sampling_pdf() -> Vec<u8> {
    let mut pdf = std::fs::read(DCT_TWO_COMPONENT).expect("read two-component DCT fixture");
    let sof = pdf
        .windows(2)
        .position(|marker| marker == [0xff, 0xc0])
        .expect("baseline JPEG must contain SOF0");
    let segment_length = u16::from_be_bytes([pdf[sof + 2], pdf[sof + 3]]) as usize;
    assert_eq!(segment_length, 14);
    assert_eq!(pdf[sof + 9], 2);
    pdf[sof + 11] = 0x31;
    pdf[sof + 14] = 0x21;
    pdf
}

#[test]
fn show_object_accepts_qpdf_selector_forms() {
    for (selector, expected) in [
        ("1", "<< /Pages 2 0 R /Type /Catalog >>"),
        ("1,0", "<< /Pages 2 0 R /Type /Catalog >>"),
        // A trailing comma with no generation digits defaults to 0, same as
        // omitting the comma entirely (qpdf's `parse_object_id`,
        // `libqpdf/QPDFJob.cc:929-940`).
        ("1,", "<< /Pages 2 0 R /Type /Catalog >>"),
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
fn tiff_predictor_row_geometry_wraps_like_qpdf_11_9_0() {
    if !qpdf_11_9_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for the TIFF predictor arithmetic oracle test");
        }
        eprintln!("qpdf 11.9.0 not available; skipping TIFF predictor arithmetic parity test");
        return;
    }

    let directory = tempfile::tempdir().expect("create TIFF predictor fixture directory");
    let path = directory.path().join("tiff-predictor-wrap.pdf");
    std::fs::write(&path, tiff_predictor_wrapped_geometry_pdf())
        .expect("write TIFF predictor fixture");
    let path_arg = path.to_str().expect("temporary path is UTF-8");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-object=3", "--filtered-stream-data"])
        .arg(&path)
        .output()
        .expect("run qpdf filtered stream inspection");
    assert_eq!(qpdf.status.code(), Some(0));
    assert_eq!(qpdf.stdout, b"abcd");
    assert!(qpdf.stderr.is_empty());

    let flpdf = flpdf(&["--show-object=3", "--filtered-stream-data", path_arg]);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "flpdf rejected qpdf 11.9.0's wrapped TIFF row width:\n{}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn show_object_two_component_dct_matches_qpdf_11_9_output_components() {
    if !qpdf_11_9_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for the two-component DCT oracle test");
        }
        eprintln!("qpdf 11.9.0 not available; skipping two-component DCT parity test");
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-object=3", "--filtered-stream-data"])
        .arg(DCT_TWO_COMPONENT)
        .output()
        .expect("run qpdf 11.9.0 on the two-component DCT fixture");
    assert_eq!(qpdf.status.code(), Some(0));
    assert_eq!(qpdf.stdout, [0x80, 0x80]);
    assert!(qpdf.stderr.is_empty(), "{:?}", qpdf.stderr);

    let flpdf = flpdf(&[
        "--show-object=3",
        "--filtered-stream-data",
        DCT_TWO_COMPONENT,
    ]);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "flpdf rejected qpdf's two output components:\n{}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(flpdf.stderr, qpdf.stderr);
}

#[test]
fn show_object_two_component_fractional_sampling_error_matches_qpdf_11_9() {
    if !qpdf_11_9_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for the DCT sampling error oracle test");
        }
        eprintln!("qpdf 11.9.0 not available; skipping DCT sampling error parity test");
        return;
    }

    let directory = tempfile::tempdir().expect("create fractional sampling fixture directory");
    let path = directory.path().join("dct-fractional-sampling.pdf");
    std::fs::write(&path, two_component_fractional_sampling_pdf())
        .expect("write fractional sampling fixture");
    let path_arg = path.to_str().expect("temporary path is UTF-8");

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-object=3", "--filtered-stream-data"])
        .arg(&path)
        .output()
        .expect("run qpdf 11.9.0 on the fractional sampling fixture");
    assert_eq!(qpdf.status.code(), Some(3));
    assert!(qpdf.stdout.is_empty());

    let flpdf = flpdf(&["--show-object=3", "--filtered-stream-data", path_arg]);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "fractional sampling status differs from qpdf:\n{}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        normalize_diagnostic_program_name(&flpdf.stderr),
        normalize_diagnostic_program_name(&qpdf.stderr)
    );
}

#[test]
fn show_object_default_dct_reserved_marker_diagnostic_matches_qpdf_11_9() {
    if !qpdf_11_9_available() {
        if std::env::var_os("CI").is_some() {
            panic!("qpdf 11.9.0 is required for the DCT marker diagnostic oracle test");
        }
        eprintln!("qpdf 11.9.0 not available; skipping DCT marker diagnostic parity test");
        return;
    }

    let qpdf = ShellCommand::new("qpdf")
        .args(["--show-object=3", "--filtered-stream-data"])
        .arg(DCT_RESERVED_MARKER)
        .output()
        .expect("run qpdf 11.9.0 on the reserved-marker DCT fixture");
    assert_eq!(qpdf.status.code(), Some(3));
    assert!(qpdf.stdout.is_empty());
    assert!(String::from_utf8_lossy(&qpdf.stderr).contains("Unsupported marker type 0x02"));

    let flpdf = flpdf(&[
        "--show-object=3",
        "--filtered-stream-data",
        DCT_RESERVED_MARKER,
    ]);
    assert_eq!(
        flpdf.status.code(),
        qpdf.status.code(),
        "reserved-marker exit differs from qpdf:\n{}",
        String::from_utf8_lossy(&flpdf.stderr)
    );
    assert_eq!(flpdf.stdout, qpdf.stdout);
    assert_eq!(
        normalize_diagnostic_program_name(&flpdf.stderr),
        normalize_diagnostic_program_name(&qpdf.stderr)
    );
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
