# --deterministic-id Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task.

> **Correction (post-review, supersedes the /ID details below).** Primary-source
> verification against qpdf 11.9.0 (source + output) showed the real algorithm
> differs from this plan's initial model: qpdf preserves `/ID[0]` (the permanent
> identifier) from the input and sets only `/ID[1]` to a digest, and that digest
> is `md5(hex(md5(output up to /ID)) + " QPDF " + /Info strings)` — not a single
> MD5 over the body with both elements equal. The shipped implementation
> therefore **preserves `/ID[0]`** and uses flpdf's own self-stable `/ID[1]`
> digest (`md5(bytes[0..xref_offset])`); it is qpdf-equivalent in *behaviour*
> (deterministic, content-derived, permanent-ID-preserving) but **not**
> byte-identical to qpdf's `/ID`. Full byte-level qpdf `/ID` parity is tracked as
> a follow-up issue. Disregard the "both `/ID` elements", "qpdf's hash range", and
> "byte-parity bonus" wording in the sections below.

**Goal:** Implement qpdf-equivalent `--deterministic-id`: derive the output `/ID` from an MD5 over the output body so it is self-stable across runs, and reject the qpdf-incompatible combinations.

**Architecture:** `write_pdf_full_rewrite` already buffers the whole output into a `Vec<u8>` and captures `xref_offset = bytes.len()` right after the body, before the xref table (`crates/flpdf/src/writer.rs:2866`). When `deterministic_id` is set we compute `MD5(bytes[0..xref_offset])` (= qpdf's hash range: header + body, excluding xref/trailer) and set both `/ID` elements to that digest, applied through `apply_encrypt_trailer_entries` (the single seam used by Table/Stream/QDF paths). deterministic-id + encryption and deterministic-id + static-id are mutually exclusive (qpdf rejects both); the linearized writer is out of scope and errors explicitly.

**Tech Stack:** Rust, `md5` crate (already a `flpdf` dependency), `clap` (CLI).

**Design source:** beads `flpdf-9hc.13` design field. Acceptance bar for deterministic-id = SELF-stability + documented qpdf-divergence (byte-parity is a feature-gated bonus, required only for `--static-id`).

**Coverage gate:** flpdf changed lines must be 100% covered — run `scripts/patch-coverage.sh --base main` after committing. Error arms (encrypt/static-id/linearize guards) need real assertions.

---

### Task 1: `WriteOptions::deterministic_id` field + static-id mutual-exclusion guard (lib)

**Files:**
- Modify: `crates/flpdf/src/writer.rs` (WriteOptions struct ~`line 188`; `write_pdf_full_rewrite` early guard ~`line 2234`)
- Test: `crates/flpdf/src/writer.rs` (`mod tests`)

**Step 1: Write the failing test**

```rust
#[test]
fn deterministic_id_and_static_id_are_mutually_exclusive() {
    let mut pdf = open_minimal_fixture(); // reuse an existing helper used by other writer tests
    let opts = WriteOptions { deterministic_id: true, static_id: true, ..Default::default() };
    let mut buf = Vec::new();
    let err = write_pdf_with_options(&mut pdf, &mut buf, &opts).unwrap_err();
    assert!(matches!(err, crate::Error::Unsupported(ref m) if m.contains("mutually exclusive")));
}
```
(Use whatever minimal-PDF builder the existing writer tests use — grep the test module for an existing `open_*`/`minimal` helper; do NOT add a new fixture.)

**Step 2: Run — expect FAIL** (`deterministic_id` field does not exist → compile error).
Run: `cargo test -p flpdf --lib deterministic_id_and_static_id_are_mutually_exclusive`

**Step 3: Implement**
- Add to `WriteOptions` (after `static_id`, keep `#[non_exhaustive]`):
```rust
    /// Derive the trailer `/ID` from an MD5 digest of the output body (header +
    /// objects, up to the cross-reference table) so the identifier is stable
    /// across runs for identical input and flags. Mirrors `qpdf --deterministic-id`.
    ///
    /// Mutually exclusive with [`WriteOptions::static_id`], and rejected for
    /// encrypted output (the `/ID` feeds the encryption key) — matching qpdf.
    /// The digest depends on the exact output bytes, so under the default
    /// Pure-Rust build the `/ID` is self-stable but NOT byte-identical to
    /// qpdf's (compressed-stream bytes differ); byte-parity holds only under
    /// the `qpdf-zlib-compat` test feature for classic-xref output.
    pub deterministic_id: bool,
```
- Near the top of `write_pdf_full_rewrite` (right after `refuse_signed_full_rewrite(...)?;`):
```rust
    if options.deterministic_id && options.static_id {
        return Err(crate::Error::Unsupported(
            "deterministic_id and static_id are mutually exclusive".to_string(),
        ));
    }
```

**Step 4: Run — expect PASS.**

**Step 5: Commit** `feat(flpdf): add WriteOptions::deterministic_id + static-id exclusion guard`

---

### Task 2: Deterministic `/ID` digest applied in full-rewrite (lib, core)

**Files:**
- Modify: `crates/flpdf/src/writer.rs` — `apply_encrypt_trailer_entries` (~`line 2021`), its two call sites (~`2903` Table, ~`3067` Stream), and a new `apply_deterministic_id` helper near `apply_static_id` (~`line 1466`).
- Test: `crates/flpdf/src/writer.rs` (`mod tests`)

**Step 1: Write the failing test (self-stability — the REQUIRED acceptance)**

```rust
#[test]
fn deterministic_id_is_stable_across_runs_and_content_dependent() {
    let opts = WriteOptions { deterministic_id: true, ..Default::default() };

    let mut a1 = open_minimal_fixture();
    let mut o1 = Vec::new();
    write_pdf_with_options(&mut a1, &mut o1, &opts).unwrap();
    let mut a2 = open_minimal_fixture();
    let mut o2 = Vec::new();
    write_pdf_with_options(&mut a2, &mut o2, &opts).unwrap();
    assert_eq!(o1, o2, "same input + deterministic_id must produce identical output");

    // /ID must equal MD5(body up to xref) and both elements are equal.
    let id = extract_trailer_id(&o1); // helper: parse the two /ID hex strings
    assert_eq!(id.0, id.1, "both /ID elements equal the digest");

    // Different content → different /ID.
    let mut b = open_other_minimal_fixture();
    let mut ob = Vec::new();
    write_pdf_with_options(&mut b, &mut ob, &opts).unwrap();
    assert_ne!(extract_trailer_id(&ob).0, id.0);
}
```
(If no `extract_trailer_id` helper exists, add a small test-only parser, or assert `o1 == o2` plus that the `/ID` bytes are NOT the static-id constant and NOT all-random by comparing two runs.)

**Step 2: Run — expect FAIL** (today the non-static path is random → `o1 != o2`).

**Step 3: Implement**
- New helper near `apply_static_id`:
```rust
/// Set the trailer `/ID` to two copies of `digest` (qpdf `--deterministic-id`
/// shape: both the permanent and changing identifiers equal the body digest).
pub(crate) fn apply_deterministic_id(trailer: &mut Dictionary, digest: [u8; 16]) {
    let s = Object::String(digest.to_vec());
    trailer.insert("ID", Object::Array(vec![s.clone(), s]));
}
```
- Compute the digest once, right after `let xref_offset = bytes.len();` (line 2866):
```rust
    let deterministic_id_digest: Option<[u8; 16]> = if options.deterministic_id {
        use md5::Digest as _;
        Some(md5::Md5::digest(&bytes[..xref_offset]).into())
    } else {
        None
    };
```
- Add a `deterministic_id: Option<[u8; 16]>` parameter to `apply_encrypt_trailer_entries` and branch in its non-encrypted arm BEFORE static/random:
```rust
        if let Some(digest) = deterministic_id {
            apply_deterministic_id(trailer, digest);
        } else if options.static_id {
            apply_static_id(trailer);
        } else {
            apply_random_id(trailer);
        }
```
- Pass `deterministic_id_digest` at both call sites (Table `~2903`, Stream `~3067`). The encrypted arm is unreachable on this path (guarded in Task 3), so it needs no digest branch.

**Step 4: Run — expect PASS.**

**Step 5: Commit** `feat(flpdf): deterministic /ID via MD5 over output body in full-rewrite`

---

### Task 3: Reject deterministic-id + encryption (lib) and + linearize (lib)

**Files:**
- Modify: `crates/flpdf/src/writer.rs` — after `let encrypting = ...;` (~`line 2252`).
- Modify: `crates/flpdf/src/linearization/writer.rs` — early in the linearized write entry point.
- Test: both modules.

**Step 1: Write failing tests**

```rust
// writer.rs tests
#[test]
fn deterministic_id_rejected_with_encryption() {
    let mut pdf = open_minimal_fixture();
    let opts = WriteOptions {
        deterministic_id: true,
        encrypt: Some(/* minimal encrypt spec used by existing encrypt tests */),
        ..Default::default()
    };
    let err = write_pdf_with_options(&mut pdf, &mut Vec::new(), &opts).unwrap_err();
    assert!(matches!(err, crate::Error::Unsupported(ref m)
        if m == "the deterministic-id option is incompatible with encrypted output files"));
}
```
Add an analogous `deterministic_id_rejected_with_linearize` test driving the linearized writer entry point (mirror an existing linearize test's setup).

**Step 2: Run — expect FAIL.**

**Step 3: Implement**
- writer.rs, right after `encrypting` is computed:
```rust
    if options.deterministic_id && encrypting {
        return Err(crate::Error::Unsupported(
            "the deterministic-id option is incompatible with encrypted output files".to_string(),
        ));
    }
```
- linearization/writer.rs, early guard:
```rust
    if options.deterministic_id {
        return Err(crate::Error::Unsupported(
            "deterministic-id is not yet supported for linearized output".to_string(),
        ));
    }
```

**Step 4: Run — expect PASS.**

**Step 5: Commit** `feat(flpdf): reject deterministic-id with encryption / linearize (qpdf parity)`

---

### Task 4: CLI `--deterministic-id` flag wiring (.13.6)

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` — `Rewrite` args (near `--static-id` ~`line 273`) and the qpdf-shaped top-level alias (near ~`line 814`); the place that builds `WriteOptions`.
- Test: `crates/flpdf-cli/tests/cli_tests.rs`

**Step 1: Write failing CLI tests**
- `--deterministic-id` produces stable output across two invocations on the same input.
- `--deterministic-id --static-id` → clap error (mutual exclusion), non-zero exit.
- `--deterministic-id --encrypt ...` → error containing `incompatible with encrypted output files`, non-zero exit.

**Step 2: Run — expect FAIL.**

**Step 3: Implement**
- Add the clap arg on the same surfaces as `--static-id`:
```rust
    /// Generate a deterministic /ID from a digest of the file contents instead
    /// of a random value (qpdf --deterministic-id equivalent).
    #[arg(long = "deterministic-id", conflicts_with = "static_id")]
    deterministic_id: bool,
```
- Map it to `WriteOptions::deterministic_id` wherever `static_id` is mapped.
- Do NOT emit the `--static-id` "testing only" stderr warning for this flag.
- Surface the writer's `Unsupported` error for the encrypt combo (single source of the qpdf message).

**Step 4: Run — expect PASS.**

**Step 5: Commit** `feat(cli): wire --deterministic-id (conflicts with --static-id; encrypt-incompatible)`

---

### Task 5: Version + ID controls test sweep (.13.7) + divergence doc + coverage

**Files:**
- Test: `crates/flpdf-cli/tests/cli_tests.rs` and/or `crates/flpdf/src/writer.rs` tests.
- Modify (docs): the `deterministic_id` doc comment (already drafted in Task 1) is the documented-divergence note; optionally add a line to the 9hc.20.6 divergence registry if one exists.

**Step 1: Add the remaining .13.7 cases** (reuse existing fixtures/helpers):
- `--min-version` on a lower-version input (bumps) and a higher-version input (no downgrade).
- `--force-version` overrides regardless of source.
- `--static-id` emits the π constant (assert exact bytes — likely already covered; add only if missing).
- JSON and QDF output with `--no-original-object-ids` on vs off.

**Step 2: Add the feature-gated byte-parity BONUS test** (only compiled under `qpdf-zlib-compat`, gated like the existing golden tests):
- flpdf `--deterministic-id` output == `qpdf --deterministic-id` output on one classic-xref fixture. Skip/feature-gate so the default build never depends on qpdf being installed.

**Step 3: Run the targeted tests — expect PASS.**
Run: `cargo test -p flpdf --lib` and `cargo test -p flpdf-cli`

**Step 4: Coverage gate**
```bash
git add -A && git commit -m "test(flpdf): version + ID controls sweep (.13.7) + det-id byte-parity (feature-gated)"
scripts/patch-coverage.sh --base main
```
Expected: flpdf changed lines 100% covered. Any genuinely untestable line → `// cov:ignore: <reason>` and note it in the PR description.

**Step 5: Final verification**
Run (in this repo's plan-doc order): `cargo build` → `cargo test -p flpdf -p flpdf-cli` → `cargo clippy -p flpdf -p flpdf-cli -- -D warnings`, plus `cargo fmt --check`.

---

### Wrap-up (after all tasks)
- File a follow-up beads issue: "deterministic-id for linearized output (qpdf writeLinearized pass-1 MD5 over body)".
- Close `flpdf-9hc.13.3`, `.13.6`, `.13.7`, then the epic `flpdf-9hc.13` if all children are done.
