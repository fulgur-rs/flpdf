# Incremental output semantic matrix implementation plan

> **For agentic workers:** execute this plan in the isolated `flpdf-25kg.6.2`
> worktree with the TDD and verification checkpoints below.

**Goal:** Add a deterministic integration matrix for flpdf incremental output
that compares the final document with a qpdf 11.9.0 full-rewrite oracle while
checking flpdf-specific appended-revision invariants.

**Architecture:** Keep the incremental gate in the CLI integration-test crate,
where qpdf is already an established CI dependency and the existing qpdf
compatibility helpers live. Each supported row mutates a real `Pdf`, writes
with `write_pdf_with_options` and a static ID, then runs qpdf twice: `--check`
on the result and a fresh full rewrite of the result. The semantic/structural
comparison uses normalized qpdf JSON snapshots; normalization removes only
incremental/xref serialization metadata and canonicalizes object references,
so no raw-byte identity is asserted. The append assertions use the public
flpdf xref API and the source-prefix rule. The encrypted-source row is an
explicit excluded policy row owned by `flpdf-9hc.29`, not a supported
qpdf-comparable tuple.

**Tech Stack:** Rust integration tests, `flpdf::{Pdf, XrefEntry, XrefForm,
WriteOptions}`, qpdf 11.9.0 CLI, `serde_json`, `tempfile`.

---

### 1. Add the failing matrix contract

**Files:**

- Add: `crates/flpdf-cli/tests/incremental_matrix_tests.rs`

Add the named matrix rows, expected support status, qpdf availability gate,
and the top-level test entry point. The first version deliberately references
the not-yet-defined case runner and invariant/oracle helpers so the focused
test fails at compile time. Include rows for classic-xref touched output,
xref-stream touched output, generated incremental ObjStm output, delete/free
output, multi-update output, warning exit behavior, and the explicitly
excluded encrypted-source policy.

Run:

```text
cargo test -p flpdf-cli --test incremental_matrix_tests
```

Expected result: RED because the matrix runner and snapshot/invariant helpers
do not exist yet.

### 2. Implement the deterministic matrix runner

**Files:**

- Modify: `crates/flpdf-cli/tests/incremental_matrix_tests.rs`

Implement deterministic source construction and mutation helpers. Build small
classic and xref-stream source variants in the integration test, including the
minimal xref-stream source needed to exercise `ObjectStreamMode::Generate`
without depending on an unrelated fixture. For each supported row:

- preserve the exact source prefix;
- write with `full_rewrite = false` and `static_id = true`;
- record the expected touched/deleted object references and values;
- assert final resolution, xref entry kind/generation, xref form, trailer
  `/Prev`, `/Size`, `/Root`, and `/ID` behavior; and
- assert at least one appended indirect-object body for the plain touched
  route, while using the public compressed-entry form for ObjStm members.

Keep multi-update checks separate enough to prove every new revision points to
the immediately previous `startxref`, not only that the final file opens.

### 3. Add the qpdf final-document oracle

**Files:**

- Modify: `crates/flpdf-cli/tests/incremental_matrix_tests.rs`

Run qpdf 11.9.0 `--check` on every supported result and create a fresh qpdf
full-rewrite output from that result. Compare normalized `--json=2` snapshots
of the incremental result and the qpdf rewrite. Remove only xref-writer
metadata that is expected to differ because qpdf has no incremental writer
(`/Prev`, `/ID`, `/Size`, xref-stream keys, and object-number spelling) and
compare the canonical reachable object graph; retain page, object,
dictionary, stream, and encryption semantics. Do not compare incremental
bytes against qpdf bytes.

Add a warning-row assertion that records qpdf/flpdf warning exit `3` and still
requires the output file to exist and be readable. Keep the encrypted-source
row as an explicit `excluded` result with `flpdf-9hc.29` as its owner.

### 4. Verify and document the CI gate

**Files:**

- Modify: `crates/flpdf-cli/tests/incremental_matrix_tests.rs` only if the
  focused verification exposes a missing assertion.

Run the focused matrix and the existing writer/CLI gates, then format and run
the workspace tests. Confirm that the existing Linux CI workflow's
`cargo test --workspace` discovers the new integration test and that no byte
identity assertion is present for incremental output.

Commands:

```text
cargo fmt -- --check
cargo test -p flpdf-cli --test incremental_matrix_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test compat_matrix_tests
cargo test -p flpdf-cli --test cli_check_exitcodes
cargo test --workspace
```

Before handoff, inspect the diff and report the isolated branch/worktree and
all verification results. Integration, Beads closure, and remote publication
remain separate decisions.
