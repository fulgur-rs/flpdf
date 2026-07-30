# Non-QDF Dictionary-Key Escaping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every generic non-QDF dictionary key through flpdf's qpdf-compatible PDF-name escaping.

**Architecture:** Keep the existing `Dictionary` serializers and their ordering rules unchanged. Add RED tests at the serializer boundary and CLI boundary, replace four raw key writes with `write_name_escaped`, then verify against qpdf 11.9.0 and the existing byte-identity suite.

**Tech Stack:** Rust 2021 workspace; existing `Dictionary`, `Object`, and `write_name_escaped`; `assert_cmd`; `tempfile`; pinned qpdf 11.9.0 source and `/usr/bin/qpdf`; Cargo tests, Clippy, `cargo llvm-cov`, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 is the source and behavioral oracle.
- Preserve `BTreeMap` ordering and the existing `/ID`, `/Length`, `/Filter`, and `/DecodeParms` special cases.
- Do not change QDF layout or introduce a second escaping helper.
- Keys that require no escaping must remain byte-identical.
- Production code may be edited only after the new tests have failed for the expected raw-key reason.
- Fresh changed executable-line coverage must be 100%.

---

## File Structure

- Modify `crates/flpdf/src/object.rs`: add four serializer-boundary regression tests and route four raw key writes through `write_name_escaped`.
- Create `crates/flpdf-cli/tests/cli_dictionary_key_escape.rs`: build one synthetic PDF and compare the escaped key tokens emitted by flpdf and qpdf 11.9.0.
- No committed binary fixture is needed; the CLI test computes exact xref offsets before writing its temporary input.

### Task 1: RED serializer and qpdf CLI regressions

**Files:**
- Modify: `crates/flpdf/src/object.rs:1042-1080`
- Create: `crates/flpdf-cli/tests/cli_dictionary_key_escape.rs`

**Interfaces:**
- Consumes: `Dictionary::{write_pdf,write_pdf_with_id_writer,write_pdf_stream,write_pdf_trailer}` and the `flpdf rewrite --static-id INPUT OUTPUT` CLI.
- Produces: four exact-output unit contracts plus one live qpdf token-parity contract.

- [ ] **Step 1: Add the four low-level failing tests**

Insert this test module after `qdf_key_escape_tests` in
`crates/flpdf/src/object.rs`:

```rust
#[cfg(test)]
mod compact_key_escape_tests {
    use super::*;

    const RAW_KEY: &[u8] = b"A B#C/D\x80E";
    const ESCAPED_KEY: &[u8] = b"A#20B#23C#2fD#80E";

    fn dictionary() -> Dictionary {
        let mut dictionary = Dictionary::new();
        dictionary.insert(RAW_KEY, Object::Integer(1));
        dictionary
    }

    #[test]
    fn plain_dictionary_key_is_name_escaped() {
        let mut out = Vec::new();
        dictionary().write_pdf(&mut out);
        assert_eq!(out, [b"<< /", ESCAPED_KEY, b" 1 >>"].concat());
    }

    #[test]
    fn id_writer_dictionary_key_is_name_escaped() {
        let mut out = Vec::new();
        dictionary().write_pdf_with_id_writer(&mut out, None);
        assert_eq!(out, [b"<< /", ESCAPED_KEY, b" 1 >>"].concat());
    }

    #[test]
    fn stream_dictionary_key_is_name_escaped() {
        let mut dictionary = dictionary();
        dictionary.insert(b"Length", Object::Integer(0));
        let mut out = Vec::new();
        dictionary.write_pdf_stream(&mut out, false);
        assert_eq!(
            out,
            [b"<< /", ESCAPED_KEY, b" 1 /Length 0 >>"].concat()
        );
    }

    #[test]
    fn trailer_dictionary_key_is_name_escaped() {
        let mut dictionary = dictionary();
        dictionary.insert(b"Size", Object::Integer(5));
        let mut out = Vec::new();
        dictionary.write_pdf_trailer(&mut out, None);
        assert_eq!(out, [b"<< /", ESCAPED_KEY, b" 1 /Size 5 >>"].concat());
    }
}
```

- [ ] **Step 2: Add the live qpdf CLI regression**

Create `crates/flpdf-cli/tests/cli_dictionary_key_escape.rs` with:

```rust
use assert_cmd::Command as CargoCommand;
use std::path::Path;
use std::process::Command as ShellCommand;
use tempfile::tempdir;

const ESCAPED_KEYS: [&[u8]; 3] = [
    b"/Catalog#20Key",
    b"/Stream#20Key",
    b"/Trailer#20Key",
];

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn qpdf_available() -> bool {
    ShellCommand::new("qpdf")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[must_use]
fn skip_if_qpdf_missing() -> bool {
    if qpdf_available() {
        return false;
    }
    if std::env::var_os("CI").is_some() {
        panic!("qpdf is required for cli_dictionary_key_escape on CI");
    }
    eprintln!("skipping: qpdf not available");
    true
}

fn write_fixture(path: &Path) {
    let objects: [&[u8]; 4] = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Catalog#20Key 7 /Payload 4 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Count 1 /Kids [3 0 R] >>\nendobj\n",
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] /Resources << >> /Contents 4 0 R >>\nendobj\n",
        b"4 0 obj\n<< /Length 0 /Stream#20Key 8 >>\nstream\nendstream\nendobj\n",
    ];
    let mut bytes = b"%PDF-1.7\n%\xbf\xf7\xa2\xfe\n".to_vec();
    let mut offsets = Vec::new();
    for object in objects {
        offsets.push(bytes.len());
        bytes.extend_from_slice(object);
    }
    let startxref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size 5 /Root 1 0 R /Trailer#20Key 9 >>\n\
             startxref\n{startxref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    std::fs::write(path, bytes).expect("write fixture");
}

#[test]
fn non_qdf_dictionary_keys_match_qpdf_name_escaping() {
    if skip_if_qpdf_missing() {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let input = temp.path().join("input.pdf");
    let flpdf_output = temp.path().join("flpdf.pdf");
    let qpdf_output = temp.path().join("qpdf.pdf");
    write_fixture(&input);

    CargoCommand::cargo_bin("flpdf")
        .expect("flpdf binary")
        .args(["rewrite", "--static-id"])
        .arg(&input)
        .arg(&flpdf_output)
        .assert()
        .success();

    let qpdf_status = ShellCommand::new("qpdf")
        .arg("--static-id")
        .arg(&input)
        .arg(&qpdf_output)
        .status()
        .expect("run qpdf 11.9.0");
    assert!(qpdf_status.success(), "qpdf --static-id failed");

    let flpdf = std::fs::read(flpdf_output).expect("read flpdf output");
    let qpdf = std::fs::read(qpdf_output).expect("read qpdf output");
    for key in ESCAPED_KEYS {
        assert!(contains(&qpdf, key), "qpdf oracle omitted {key:?}");
        assert!(
            contains(&flpdf, key),
            "flpdf did not match qpdf escaping for {key:?}"
        );
    }
}
```

- [ ] **Step 3: Run both test boundaries and verify RED**

Run:

```bash
cargo test -p flpdf compact_key_escape_tests -- --nocapture
cargo test -p flpdf-cli --test cli_dictionary_key_escape -- --nocapture
```

Expected:

- all four unit tests fail with actual output containing raw
  `A B#C/D\x80E`;
- the qpdf half of the CLI test contains all three escaped tokens;
- the flpdf half fails because at least `/Catalog#20Key` is absent.

If the qpdf output omits one of the synthetic keys, adjust only the fixture's
reachability or assertion set and rerun until the oracle half passes while
flpdf still fails for raw-key output.

### Task 2: GREEN four non-QDF serializers

**Files:**
- Modify: `crates/flpdf/src/object.rs:834-970`
- Test: `crates/flpdf/src/object.rs`
- Test: `crates/flpdf-cli/tests/cli_dictionary_key_escape.rs`

**Interfaces:**
- Consumes: `write_name_escaped(out: &mut Vec<u8>, raw: &[u8])`.
- Produces: unchanged serializer APIs whose dictionary-key tokens match qpdf 11.9.0.

- [ ] **Step 1: Replace the four raw key writes**

In each loop below, keep the existing `b" /"` prefix and following space, but
replace `out.extend_from_slice(key)` with `write_name_escaped(out, key)`:

```rust
// Dictionary::write_pdf
out.extend_from_slice(b" /");
write_name_escaped(out, key);
out.push(b' ');

// Dictionary::write_pdf_with_id_writer
out.extend_from_slice(b" /");
write_name_escaped(out, key);
out.push(b' ');

// Dictionary::write_pdf_stream
out.extend_from_slice(b" /");
write_name_escaped(out, key);
out.push(b' ');

// Dictionary::write_pdf_trailer
out.extend_from_slice(b" /");
write_name_escaped(out, key);
out.push(b' ');
```

Do not change the literal `/Length`, `/Filter`, or `/ID` emissions: those names
contain no escapable bytes and their placement is part of qpdf byte parity.

- [ ] **Step 2: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf compact_key_escape_tests -- --nocapture
cargo test -p flpdf-cli --test cli_dictionary_key_escape -- --nocapture
cargo test -p flpdf stream_dict_order_tests -- --nocapture
cargo test -p flpdf qdf_key_escape_tests -- --nocapture
```

Expected: all tests pass and the qpdf CLI regression finds all three escaped
tokens in both outputs.

- [ ] **Step 3: Refactor review**

Inspect:

```bash
git diff -- crates/flpdf/src/object.rs crates/flpdf-cli/tests/cli_dictionary_key_escape.rs
```

Expected: only tests plus four helper calls; no new helper, allocation, ordering
change, or unrelated cleanup.

- [ ] **Step 4: Format and commit the tested fix**

Run:

```bash
cargo fmt --all
git diff --check
git add crates/flpdf/src/object.rs crates/flpdf-cli/tests/cli_dictionary_key_escape.rs
git commit -m "fix(writer): escape non-QDF dictionary keys"
```

Expected: one implementation commit with a clean worktree.

### Task 3: Full verification and durable handoff

**Files:**
- Verify: `crates/flpdf/src/object.rs`
- Verify: `crates/flpdf-cli/tests/cli_dictionary_key_escape.rs`
- Verify: existing workspace tests and byte-identity goldens

**Interfaces:**
- Consumes: the committed Task 2 implementation.
- Produces: formatting, lint, test, oracle, and 100% patch-coverage evidence.

- [ ] **Step 1: Run formatting and focused qpdf byte gates**

```bash
cargo fmt --all -- --check
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf-cli --test cli_dictionary_key_escape
```

Expected: all commands pass.

- [ ] **Step 2: Run workspace lint and tests**

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
```

Expected: zero warnings and zero failures.

- [ ] **Step 3: Measure fresh changed-line coverage**

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/patch-cov.lcov
scripts/patch-coverage.sh --base origin/main --lcov target/patch-cov.lcov
```

Expected: `patch-coverage: OK` with 100% changed executable-line coverage.

- [ ] **Step 4: Review final scope**

```bash
git status --short --branch
git diff --stat origin/main...HEAD
git log --oneline --decorate origin/main..HEAD
```

Expected: only the design, plan, four production call-site changes, and their
tests are present.

- [ ] **Step 5: Persist Beads and Git state**

```bash
bd close flpdf-n9t0.7 --reason="All generic non-QDF dictionary serializers now escape keys through write_name_escaped; qpdf 11.9.0 oracle tests, workspace gates, and 100% patch coverage pass."
bd dolt push
git fetch --prune origin
git rebase origin/main
git push -u origin fix/flpdf-n9t0-7-dict-key-escape
```

If the rebase changes commits, rerun the focused tests and patch coverage
before pushing. Report the pushed branch and verification evidence.
