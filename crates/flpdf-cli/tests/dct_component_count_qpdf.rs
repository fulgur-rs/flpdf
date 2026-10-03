use std::path::Path;
use std::process::{Command, Output};

fn qpdf_is_available() -> bool {
    let version = Command::new("qpdf").arg("--version").output().ok();
    if version.as_ref().is_some_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).lines().next() == Some("qpdf version 11.9.0")
    }) {
        return true;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf 11.9.0 is required for DCT component-count parity: {version:?}");
    }
    eprintln!("skipping: qpdf 11.9.0 is not available: {version:?}");
    false
}

fn filtered_data(program: &str, fixture: &Path) -> Output {
    Command::new(program)
        .args(["--show-object=3", "--filtered-stream-data"])
        .arg(fixture)
        .output()
        .expect("run filtered-stream-data command")
}

#[test]
fn default_dct_decodes_five_and_ten_component_frames_like_qpdf() {
    if !qpdf_is_available() {
        return;
    }

    for (components, pixels, fixture) in [
        (
            5,
            1,
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/dct-5-component.pdf"
            ),
        ),
        (
            5,
            64,
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/dct-5-component-progressive.pdf"
            ),
        ),
        (
            10,
            1,
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/dct-10-component.pdf"
            ),
        ),
    ] {
        let fixture = Path::new(fixture);
        let qpdf = filtered_data("qpdf", fixture);
        assert_eq!(qpdf.status.code(), Some(0), "qpdf {components}: {qpdf:?}");
        assert_eq!(qpdf.stdout.len(), components * pixels);
        assert!(qpdf
            .stdout
            .chunks_exact(components)
            .any(|pixel| pixel[..4] != [0x80; 4]));
        assert!(qpdf
            .stdout
            .chunks_exact(components)
            .all(|pixel| pixel[4..].iter().all(|sample| *sample == 0x80)));
        assert!(qpdf.stderr.is_empty(), "qpdf {components}: {qpdf:?}");

        let flpdf = filtered_data(env!("CARGO_BIN_EXE_flpdf"), fixture);
        assert_eq!(
            flpdf.status.code(),
            qpdf.status.code(),
            "flpdf {components}: {flpdf:?}; qpdf: {qpdf:?}"
        );
        assert_eq!(flpdf.stdout, qpdf.stdout, "component count {components}");
        assert_eq!(flpdf.stderr, qpdf.stderr, "component count {components}");
    }
}

#[test]
fn default_dct_rejects_frames_over_libjpeg_limit_like_qpdf() {
    if !qpdf_is_available() {
        return;
    }

    let fixture = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dct-11-component.pdf"
    ));
    let qpdf = filtered_data("qpdf", fixture);
    assert_eq!(qpdf.status.code(), Some(3), "qpdf: {qpdf:?}");
    assert!(qpdf.stdout.is_empty(), "qpdf: {qpdf:?}");
    let qpdf_stderr = String::from_utf8_lossy(&qpdf.stderr);
    assert!(
        qpdf_stderr.contains("Too many color components: 11, max 10"),
        "qpdf diagnostic: {qpdf_stderr}"
    );

    let flpdf = filtered_data(env!("CARGO_BIN_EXE_flpdf"), fixture);
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "flpdf: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    let normalize_tool = |stderr: &[u8]| {
        String::from_utf8_lossy(stderr)
            .replace("qpdf: operation", "<tool>: operation")
            .replace("flpdf: operation", "<tool>: operation")
    };
    assert_eq!(normalize_tool(&flpdf.stderr), normalize_tool(&qpdf.stderr));
}

#[test]
fn default_dct_matches_qpdf_sos_component_lookup_behavior() {
    if !qpdf_is_available() {
        return;
    }

    let fixture = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dct-5-component-sos-id-five.pdf"
    ));
    let qpdf = filtered_data("qpdf", fixture);
    let flpdf = filtered_data(env!("CARGO_BIN_EXE_flpdf"), fixture);
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "flpdf: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    let normalize_tool = |stderr: &[u8]| {
        String::from_utf8_lossy(stderr)
            .replace("qpdf: operation", "<tool>: operation")
            .replace("flpdf: operation", "<tool>: operation")
    };
    assert_eq!(normalize_tool(&flpdf.stderr), normalize_tool(&qpdf.stderr));
    if qpdf.status.code() == Some(3) {
        assert!(qpdf.stdout.is_empty(), "qpdf: {qpdf:?}");
        assert!(
            String::from_utf8_lossy(&qpdf.stderr).contains("Invalid component ID 5 in SOS"),
            "qpdf diagnostic: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
    } else {
        assert_eq!(qpdf.status.code(), Some(0), "qpdf: {qpdf:?}");
        assert_eq!(qpdf.stdout.len(), 5);
        assert!(qpdf.stderr.is_empty(), "qpdf: {qpdf:?}");
    }
}

#[test]
fn default_dct_rejects_sos_with_more_than_four_components_like_qpdf() {
    if !qpdf_is_available() {
        return;
    }

    let fixture = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dct-10-component-sos-count-five.pdf"
    ));
    let qpdf = filtered_data("qpdf", fixture);
    assert_eq!(qpdf.status.code(), Some(3), "qpdf: {qpdf:?}");
    assert!(qpdf.stdout.is_empty(), "qpdf: {qpdf:?}");
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("Bogus marker length"),
        "qpdf diagnostic: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let flpdf = filtered_data(env!("CARGO_BIN_EXE_flpdf"), fixture);
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "flpdf: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    let normalize_tool = |stderr: &[u8]| {
        String::from_utf8_lossy(stderr)
            .replace("qpdf: operation", "<tool>: operation")
            .replace("flpdf: operation", "<tool>: operation")
    };
    assert_eq!(normalize_tool(&flpdf.stderr), normalize_tool(&qpdf.stderr));
}

#[test]
fn default_dct_rejects_malformed_sos_length_like_qpdf() {
    if !qpdf_is_available() {
        return;
    }

    let fixture = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dct-10-component-sos-bad-length.pdf"
    ));
    let qpdf = filtered_data("qpdf", fixture);
    assert_eq!(qpdf.status.code(), Some(3), "qpdf: {qpdf:?}");
    assert!(qpdf.stdout.is_empty(), "qpdf: {qpdf:?}");
    assert!(
        String::from_utf8_lossy(&qpdf.stderr).contains("Bogus marker length"),
        "qpdf diagnostic: {}",
        String::from_utf8_lossy(&qpdf.stderr)
    );

    let flpdf = filtered_data(env!("CARGO_BIN_EXE_flpdf"), fixture);
    assert_eq!(flpdf.status.code(), qpdf.status.code(), "flpdf: {flpdf:?}");
    assert_eq!(flpdf.stdout, qpdf.stdout);
    let normalize_tool = |stderr: &[u8]| {
        String::from_utf8_lossy(stderr)
            .replace("qpdf: operation", "<tool>: operation")
            .replace("flpdf: operation", "<tool>: operation")
    };
    assert_eq!(normalize_tool(&flpdf.stderr), normalize_tool(&qpdf.stderr));
}
