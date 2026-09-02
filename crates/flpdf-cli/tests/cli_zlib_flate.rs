use assert_cmd::Command;
use std::io::Write;
use std::process::{Command as ProcessCommand, Output, Stdio};

#[path = "support/text.rs"]
mod text;
use text::EOL;

fn run(binary: &str, args: &[&str], input: &[u8]) -> Output {
    Command::cargo_bin(binary)
        .unwrap()
        .args(args)
        .write_stdin(input)
        .output()
        .unwrap()
}

fn qpdf_available() -> bool {
    ProcessCommand::new("/usr/bin/zlib-flate")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_qpdf(args: &[&str], input: &[u8]) -> Output {
    let mut child = ProcessCommand::new("/usr/bin/zlib-flate")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn zlib_flate_subcommand_compresses_and_uncompresses_stdin() {
    let input = b"raw zlib data\nraw zlib data\n";
    let compressed = run("flpdf", &["zlib-flate", "-compress"], input);
    assert_eq!(compressed.status.code(), Some(0));
    assert_ne!(compressed.stdout, input);

    let uncompressed = run("flpdf", &["zlib-flate", "-uncompress"], &compressed.stdout);
    assert_eq!(uncompressed.status.code(), Some(0));
    assert_eq!(uncompressed.stdout, input);
}

#[test]
fn zlib_flate_standalone_alias_uses_the_same_handler() {
    let input = b"standalone zlib-flate alias\n";
    let compressed = run("zlib-flate", &["-compress"], input);
    assert_eq!(compressed.status.code(), Some(0));

    let uncompressed = run("zlib-flate", &["-uncompress"], &compressed.stdout);
    assert_eq!(uncompressed.status.code(), Some(0));
    assert_eq!(uncompressed.stdout, input);
}

#[test]
fn zlib_flate_version_and_usage_match_qpdf_shape() {
    let version = run("zlib-flate", &["--version"], b"");
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(
        version.stdout,
        format!("zlib-flate from qpdf version 11.9.0{EOL}").into_bytes()
    );

    let usage = run("flpdf", &["zlib-flate"], b"");
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stderr)
        .contains("Usage: flpdf zlib-flate { -uncompress | -compress[=n] }"));
}

#[test]
fn zlib_flate_reports_qpdf_warning_status_for_truncated_input() {
    // qpdf 11.9.0's Pl_Flate emits this warning when the valid output prefix
    // is available but the zlib stream cannot finish (`Pl_Flate.cc:145-162`).
    let output = run(
        "flpdf",
        &["zlib-flate", "-uncompress"],
        &[0x78, 0x9c, 0x4b, 0x04],
    );
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"a");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("flpdf: WARNING: zlib code -5, msg = input stream is complete but output may still be valid")
    );
}

#[test]
fn zlib_flate_compression_modes_match_live_qpdf() {
    if !qpdf_available() {
        return;
    }
    let input =
        b"Once upon a time there lived three qowws.  They did not like porridge.\n".repeat(1000);
    for mode in [
        "-compress",
        "-compress=0",
        "-compress=1",
        "-compress=9",
        "-compress=-1",
        "-compress=abc",
    ] {
        let qpdf = run_qpdf(&[mode], &input);
        assert!(
            qpdf.status.success(),
            "qpdf zlib-flate {mode} failed: {}",
            String::from_utf8_lossy(&qpdf.stderr)
        );
        let flpdf = run("flpdf", &["zlib-flate", mode], &input);
        assert_eq!(flpdf.status.code(), Some(0), "mode={mode}");
        if cfg!(feature = "qpdf-zlib-compat") {
            assert_eq!(
                flpdf.stdout, qpdf.stdout,
                "qpdf-zlib-compat must make zlib-flate bytes match qpdf for {mode}"
            );
        } else {
            let qpdf_decoded = run_qpdf(&["-uncompress"], &qpdf.stdout);
            let flpdf_decoded = run("flpdf", &["zlib-flate", "-uncompress"], &flpdf.stdout);
            assert_eq!(qpdf_decoded.status.code(), Some(0), "mode={mode}");
            assert_eq!(flpdf_decoded.status.code(), Some(0), "mode={mode}");
            assert_eq!(qpdf_decoded.stdout, input, "qpdf decode for {mode}");
            assert_eq!(flpdf_decoded.stdout, input, "flpdf decode for {mode}");
        }
    }
}

#[test]
fn zlib_flate_rejects_bad_input_and_invalid_argument_shapes() {
    let bad_input = run("flpdf", &["zlib-flate", "-uncompress"], b"not zlib");
    assert_eq!(bad_input.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad_input.stderr)
        .contains("flpdf: flate: inflate: data: incorrect header check"));

    let invalid_level = run("flpdf", &["zlib-flate", "-compress=10"], b"payload");
    assert_eq!(invalid_level.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_level.stderr)
        .contains("flpdf: flate: deflate: Init: zlib stream error"));

    let empty_invalid_level = run("flpdf", &["zlib-flate", "-compress=10"], b"");
    assert_eq!(empty_invalid_level.status.code(), Some(0));
    assert!(empty_invalid_level.stdout.is_empty());

    let alias_invalid_level = run("zlib-flate", &["-compress=10"], b"payload");
    assert_eq!(alias_invalid_level.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&alias_invalid_level.stderr)
        .contains("zlib-flate: flate: deflate: Init: zlib stream error"));

    let invalid_selector = run(
        "flpdf",
        &["zlib-flate", "-compress=9223372036854775808"],
        b"",
    );
    assert_eq!(invalid_selector.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_selector.stderr)
        .contains("flpdf: overflow/underflow converting 9223372036854775808 to 64-bit integer"));

    let unknown_mode = run("flpdf", &["zlib-flate", "-unknown"], b"");
    assert_eq!(unknown_mode.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown_mode.stderr).contains("Usage: flpdf zlib-flate"));

    let extra = run("flpdf", &["zlib-flate", "-compress", "extra"], b"");
    assert_eq!(extra.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&extra.stderr).contains("Usage: flpdf zlib-flate"));
}
