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
//! by framing differences. The CLI writer appends a single newline before
//! `endstream` (its `--newline-before-endstream` modes never reproduce qpdf's
//! `Never`), so the captured payload is normalized by stripping one trailing
//! newline before comparison. The comparison stays on the raw encoded bytes
//! (the only observable difference between preserve and recompress): decoding
//! both would yield identical content and defeat the test.

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

/// Strip one trailing newline (`\n`, optionally preceded by `\r`) that the CLI
/// writer appends before `endstream`. Applied symmetrically: the source fixture
/// has no such newline, so this is a no-op there, but it normalizes the framing
/// the writer adds to the output payload.
fn strip_trailing_newline(mut v: Vec<u8>) -> Vec<u8> {
    if v.last() == Some(&b'\n') {
        v.pop();
        if v.last() == Some(&b'\r') {
            v.pop();
        }
    }
    v
}

fn source_payload() -> Vec<u8> {
    strip_trailing_newline(largest_stream_payload(
        &std::fs::read(fixture_path()).unwrap(),
    ))
}

fn output_payload(out: &[u8]) -> Vec<u8> {
    strip_trailing_newline(largest_stream_payload(out))
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
