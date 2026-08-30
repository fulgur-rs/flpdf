# Remove Stale `dead_code` Allowances Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove migration-era `#[allow(dead_code)]` annotations and obsolete cutover comments left after the ObjectHandle consumer migration, while retaining only exceptions with a current, responsibility-based reason.

**Architecture:** Treat the current canonical ObjectHandle graph as the source of truth. Build a source census and call-graph classification first, then remove one bounded file/group at a time; test-only helpers become `#[cfg(test)]` or are deleted, and production items lose the allowance only when their real consumers are proven. A Python source guard prevents completed cutover issue IDs and stale migration rationales from returning.

**Tech Stack:** Rust workspace, Cargo check/clippy/rustdoc/tests, Python `unittest`, pinned qpdf 11.9.0 source and existing qpdf differential scripts.

---

### Task 1: Establish the allowance census and ownership classification

**Files:**
- Read: `crates/flpdf/src/object_handle.rs`
- Read: `crates/flpdf/src/writer/object.rs`
- Read: `crates/flpdf/src/tokenizer.rs`
- Read: `crates/flpdf/src/acroform_document_helper.rs`
- Read: `crates/flpdf/src/reader/resolver.rs`
- Read: all remaining Rust files matched by the census
- Read: `docs/superpowers/specs/2026-08-30-qpdf-objecthandle-no-materialize-design.md`

- [ ] **Step 1: Capture the baseline census**

Run:

```bash
git grep -n -E '#\\[allow\\([^]]*dead_code' HEAD -- '*.rs'
git grep -l -E '#\\[allow\\([^]]*dead_code' HEAD -- '*.rs' | wc -l
git grep -n -E '#\\[allow\\([^]]*dead_code' HEAD -- crates/flpdf/src | wc -l
```

Expected baseline: 166 matching attributes across 40 Rust files, including 156 in `crates/flpdf/src`.

- [ ] **Step 2: Map every allowance to its attached item and callers**

For each match, inspect the item immediately below it and run symbol-specific searches:

```bash
rg -n -B3 -A2 '#\\[allow\\([^]]*dead_code' crates/flpdf/src crates/flpdf-cli/src crates/flpdf-qtest-tools/src
rg -n 'share_value_state_with|remove_from_document|promote_to_indirect|try_get_keys|try_is_name_and_equals|try_is_or_has_name|try_is_dictionary_of_type|try_array_len|try_array_item|try_as_integer|try_get_int_value|try_get_int_value_as_int|try_get_key|pipe_stream_data' crates/flpdf/src
```

Classify each item as `production-used`, `test-only`, `intentionally-unused`, or `stale-migration`. Record completed issue references separately; the CLOSED issues `flpdf-25kg.3.5`, `.3.6`, `.3.6.3`, `.3.12`, `.3.25`, and `flpdf-egzr.3.2.5` cannot remain as future-consumer rationales.

- [ ] **Step 3: Confirm qpdf responsibility before deleting a canonical primitive**

Read the pinned source without modifying it:

```bash
qpdf_source=$(scripts/fetch-qpdf-source.sh --print-path)
rg -n 'QPDFObjectHandle|QPDFValue|QPDFObject|unparseObject|pipeStreamData|resolve' "$qpdf_source/libqpdf" | head -200
```

Do not change semantics or add an adapter merely to silence a lint. A retained allowance must name a current qpdf responsibility or an explicit test-only boundary.

### Task 2: Add the RED guard for stale migration rationales

**Files:**
- Create: `scripts/tests/test_dead_code_allowances.py`
- Test: `scripts/tests/test_dead_code_allowances.py`

- [ ] **Step 1: Write the failing source guard**

```python
import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ALLOW_RE = re.compile(r"#\\[allow\\([^]]*dead_code")
STALE_MARKERS = (
    "production callers land",
    "consumer cutover",
    "deferred consumers",
    "after this prerequisite lands",
    "future ObjectHandle writer route",
    "not-yet-wired",
    "flpdf-25kg.3.5",
    "flpdf-25kg.3.6",
    "flpdf-25kg.3.6.3",
    "flpdf-25kg.3.12",
    "flpdf-25kg.3.25",
    "flpdf-egzr.3.2.5",
)


class DeadCodeAllowanceTests(unittest.TestCase):
    def test_no_dead_code_allowance_uses_completed_cutover_rationale(self):
        offenders = []
        for path in sorted((ROOT / "crates").rglob("*.rs")):
            lines = path.read_text(encoding="utf-8").splitlines()
            for index, line in enumerate(lines):
                if not ALLOW_RE.search(line):
                    continue
                context = "\\n".join(lines[index : index + 3]).lower()
                for marker in STALE_MARKERS:
                    if marker.lower() in context:
                        offenders.append(f"{path.relative_to(ROOT)}:{index + 1}: {marker}")
        self.assertEqual([], offenders, "stale dead_code rationale(s):\\n" + "\\n".join(offenders))


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run the guard and verify RED**

Run:

```bash
python3 -m unittest scripts/tests/test_dead_code_allowances.py
```

Expected: FAIL, identifying the existing completed-cutover comments in `object_handle.rs`, `writer/object.rs`, `reader/resolver.rs`, and `tokenizer.rs`.

### Task 3: Remove stale allowances in bounded canonical groups

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Modify: `crates/flpdf/src/writer/object.rs`
- Modify: `crates/flpdf/src/reader/resolver.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/tokenizer.rs`
- Modify: `crates/flpdf/src/acroform_document_helper.rs`
- Modify: any additional file whose classification is `stale-migration`

- [ ] **Step 1: Remove allowances from production-used items**

For each classified `production-used` item, delete only its `#[allow(dead_code)]` component and update the adjacent comment so it describes the current consumer. Do not alter signatures, resolution order, warning behavior, or writer bytes.

- [ ] **Step 2: Convert test-only items without changing production visibility**

For an item used only by unit tests, choose the smallest source-preserving form:

```rust
#[cfg(test)]
fn test_only_helper(...) -> ... {
    ...
}
```

If the item has no remaining caller, delete the item and its tests together. Do not add a compatibility wrapper solely to keep an old test compiling.

- [ ] **Step 3: Run the RED guard and focused Rust checks after each group**

Run the relevant focused checks after each bounded edit:

```bash
python3 -m unittest scripts/tests/test_dead_code_allowances.py
cargo check -p flpdf --lib
cargo test -p flpdf --lib object_handle
cargo test -p flpdf --lib writer::object
cargo test -p flpdf --lib reader::resolver
cargo test -p flpdf --lib tokenizer
```

Expected: the Python guard becomes GREEN, each focused Rust suite passes, and no new warning is hidden by a replacement allowance.

### Task 4: Verify the final census and parity gates

**Files:**
- Modify: `scripts/tests/test_dead_code_allowances.py` if the final guard needs a narrowly justified current exception list
- Test: all existing workspace and qpdf differential suites

- [ ] **Step 1: Capture the final census**

Run the baseline commands from Task 1 and record the final total, per-file counts, every retained allowance, and its current reason in the Beads issue. No retained line may cite a completed consumer cutover as future work.

- [ ] **Step 2: Run repository quality gates**

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features -- --test-threads=8
python3 scripts/qpdf-module-docs.py --check
python3 scripts/check-qpdf-deviation-markers.py --check
scripts/patch-coverage.sh --base feature/flpdf-objecthandle-final-removal
```

- [ ] **Step 3: Re-run the high-signal qpdf differential**

```bash
bash scripts/qpdf-test-driver-diff.sh --check
bash scripts/qpdf-objecthandle-uniform-identity-probe.sh
bash scripts/qpdf-stream-data-provider-probe.sh
bash scripts/qpdf-json-pipeline-diff.sh
bash scripts/qpdf-stream-codecs-diff.sh
```

Expected: qpdf 11.9.0 output and warning behavior remain unchanged, and fresh parent-relative patch coverage reports zero uncovered changed lines.

### Task 5: Persist and hand off the stacked cleanup

**Files:**
- Modify: Beads notes for `flpdf-25kg.3.48.7`
- Create: stacked feature branch/PR metadata only after verification

- [ ] **Step 1: Record the census and verification evidence**

Append the final head/base, final allowance census, retained exception rationale, focused tests, full gates, qpdf probes, and `bd dep cycles` result to the issue.

- [ ] **Step 2: Rebase and push the stacked branch**

```bash
git fetch origin
git rebase feature/flpdf-objecthandle-final-removal
git push --force-with-lease origin feature/flpdf-25kg.3.48.7-dead-code-cleanup
```

- [ ] **Step 3: Keep integration separate**

Create a Draft PR based on `feature/flpdf-objecthandle-final-removal`, wait for all CI checks to pass, then mark it Ready only after the green readback. Do not merge in this session; finish with `bd dolt push` and verify `Push complete.`.
