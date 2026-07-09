//! CLI E2E test for `rewrite --recompress-flate`.
//!
//! Mirrors the library-level `lone_flate_preserve_tests.rs`: a fixture whose
//! page content stream is an already-lone `/FlateDecode` is rewritten through
//! the CLI's full-rewrite path, and the largest `stream ... endstream` payload
//! of the output is compared against the source.
//!
//!   default (no --recompress-flate) → payload preserved verbatim (qpdf parity)
//!   --recompress-flate              → payload re-encoded (differs from source)
//!
//! Both invocations are byte-identical except for the added flag, so the `!=`
//! assertion can only be explained by re-encoding the *raw encoded* bytes — not
//! by framing differences. The CLI's default `--newline-before-endstream=never`
//! matches qpdf: no newline is inserted between the payload and `endstream`, so
//! the captured payload IS the raw encoded stream. The comparison stays on the
//! raw encoded bytes (the only observable difference between preserve and
//! recompress): decoding both would yield identical content and defeat the test.

use assert_cmd::Command;
use std::path::Path;

const FIXTURE: &str = "lone-flate-l9.pdf";

fn fixture_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(FIXTURE)
}

/// Return the bytes of the largest `stream ... endstream` payload — the page
/// content stream in this single-page fixture.
fn largest_stream_payload(data: &[u8]) -> Vec<u8> {
    let needle = b"stream\n";
    let mut best: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while let Some(rel) = data[i..].windows(needle.len()).position(|w| w == needle) {
        let s = i + rel + needle.len();
        let e = s + data[s..]
            .windows(b"endstream".len())
            .position(|w| w == b"endstream")
            .expect("endstream must follow stream");
        if e - s > best.len() {
            best = data[s..e].to_vec();
        }
        i = e + b"endstream".len();
    }
    best
}

fn source_payload() -> Vec<u8> {
    largest_stream_payload(&std::fs::read(fixture_path()).unwrap())
}

fn output_payload(out: &[u8]) -> Vec<u8> {
    largest_stream_payload(out)
}

/// Run `flpdf rewrite <base args> <extra> <in> <out>` and return the output PDF
/// bytes. The full-rewrite path is forced via `--full-rewrite` so the lone-Flate
/// preserve gate (which lives in the full-rewrite writer) is active. The two
/// invocations differ only by `extra_args`, keeping the `!=` assertion honest.
fn rewrite(extra_args: &[&str]) -> Vec<u8> {
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.pdf");

    let mut cmd = Command::cargo_bin("flpdf").unwrap();
    cmd.arg("rewrite").arg("--full-rewrite").arg("--static-id");
    for a in extra_args {
        cmd.arg(a);
    }
    cmd.arg(fixture_path()).arg(&out_path).assert().success();

    std::fs::read(&out_path).unwrap()
}

/// Default full rewrite preserves an already-lone `/FlateDecode` stream verbatim.
#[test]
fn cli_full_rewrite_preserves_lone_flate_verbatim() {
    let out = rewrite(&[]);
    assert_eq!(
        output_payload(&out),
        source_payload(),
        "default rewrite must preserve a lone /FlateDecode content stream verbatim"
    );
}

/// `--recompress-flate` re-encodes the lone `/FlateDecode` stream so its payload
/// no longer matches the level-9 source bytes; it must remain a lone FlateDecode.
#[test]
fn cli_recompress_flate_reencodes_lone_flate() {
    let out = rewrite(&["--recompress-flate"]);
    let payload = output_payload(&out);
    assert_ne!(
        payload,
        source_payload(),
        "--recompress-flate must re-encode the lone /FlateDecode stream (payload differs from source)"
    );
    assert!(
        out.windows(b"/Filter /FlateDecode".len())
            .any(|w| w == b"/Filter /FlateDecode"),
        "re-encoded stream must still declare a single /FlateDecode filter"
    );
}

/// Regression: `--recompress-flate` must take effect even when nothing else
/// forces a full rewrite. `--remove-unreferenced-resources=no` disables the
/// auto full-rewrite promotion, and no `--full-rewrite`/`--qdf`/`--deterministic-id`
/// is passed, so without the flag's own full-rewrite promotion the write would
/// take the incremental path and copy the stream verbatim — silently ignoring
/// `--recompress-flate`. Here the stream must be re-encoded (payload differs from
/// the level-9 source). Only the stream payload is compared, so no `--static-id`
/// is needed.
#[test]
fn cli_recompress_flate_promotes_to_full_rewrite_when_not_otherwise_forced() {
    let temp = tempfile::tempdir().unwrap();
    let out_path = temp.path().join("out.pdf");
    Command::cargo_bin("flpdf")
        .unwrap()
        .arg("rewrite")
        .arg("--remove-unreferenced-resources=no")
        .arg("--recompress-flate")
        .arg(fixture_path())
        .arg(&out_path)
        .assert()
        .success();
    let out = std::fs::read(&out_path).unwrap();
    assert_ne!(
        output_payload(&out),
        source_payload(),
        "--recompress-flate must re-encode the stream even without an explicit \
         full-rewrite trigger (it should promote to a full rewrite)"
    );
}
