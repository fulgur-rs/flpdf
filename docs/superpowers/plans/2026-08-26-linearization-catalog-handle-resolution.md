# Linearization Catalog Handle Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the active linearization writer use qpdf-shaped live Catalog handles and accept indirect `/Extensions` by preparing the graph before planning.

**Architecture:** Add one pre-plan Catalog preparation helper that translates qpdf's `prepareFileForWrite` directization using existing `ObjectHandle` primitives. Move the extension snapshot to surround that preparation, then make the outline and ADBE inspection helpers handle-only; keep output mutation and hint arithmetic on their existing writer boundaries.

**Tech Stack:** Rust workspace, `ObjectHandle`, `PdfWriter`, qpdf 11.9.0 source/live probes, cargo tests, qpdf-zlib-compatible byte goldens.

---

### Task 1: Add RED route and behavior tests

**Files:**
- Modify: `crates/flpdf/tests/linearization_route_contract_tests.rs`
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs`
- Create: `tests/fixtures/compat/linearize-indirect-extensions.pdf`
- Modify: `tests/golden/regenerate.sh`
- Create: `tests/golden/references/linearize-indirect-extensions/linearize.pdf`

- [x] **Step 1: Add the route contract before production edits.**

Extract `compute_outline_hint_info` and `resolve_catalog_adbe_status` by their
function markers. Assert that their production slices contain the canonical
`get_object_handle`, `resolve`, `try_get_key`, and `try_as_dictionary` markers
where applicable, and contain none of `resolve_borrowed`,
`Object::Dictionary`, `Object::Reference`, or `.as_dict()`.

- [x] **Step 2: Add a real one-page indirect-extension fixture and qpdf golden command.**

The fixture must contain a valid one-page `/Pages` tree and a Catalog with
`/Extensions 4 0 R`; object 4 contains `/ADBE` and `/XYZW`. Add its golden
generation beside the existing linearization commands:

```bash
qpdf --linearize --deterministic-id --warning-exit-0 \
  tests/fixtures/compat/linearize-indirect-extensions.pdf \
  tests/golden/references/linearize-indirect-extensions/linearize.pdf
```

- [x] **Step 3: Add the byte-parity integration test.**

Use the existing `flpdf_linearized` and `assert_linearize_byte_identical`
helpers in `cmp_linearize_tests.rs`:

```rust
#[test]
fn indirect_extensions_linearized_is_byte_identical_to_qpdf() {
    assert_linearize_byte_identical(
        "linearize-indirect-extensions.pdf",
        "linearize-indirect-extensions",
    );
}
```

- [x] **Step 4: Run the new tests and record the expected RED failure.**

```bash
cargo test -p flpdf --test linearization_route_contract_tests linearization_catalog_resolution_uses_live_handles
cargo test -p flpdf --test cmp_linearize_tests indirect_extensions_linearized_is_byte_identical_to_qpdf --features qpdf-zlib-compat
```

Expected: the route contract fails on the existing raw resolver, and the byte
test fails because the current writer rejects or mis-handles the indirect
`/Extensions` graph. Fix test setup errors until these are the feature-gap
failures, then commit:

```bash
git add crates/flpdf/tests/linearization_route_contract_tests.rs crates/flpdf/tests/cmp_linearize_tests.rs tests/fixtures/compat/linearize-indirect-extensions.pdf tests/golden/regenerate.sh tests/golden/references/linearize-indirect-extensions/linearize.pdf
git commit -m "test: expose linearization catalog raw route"
```

### Task 2: Implement qpdf pre-plan Catalog preparation

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs:3178-3219`
- Test: `crates/flpdf/src/linearization/writer.rs` focused unit module

- [x] **Step 1: Implement the minimal canonical preparation helper.**

Resolve the live root handle and use this exact operation order:

```rust
fn prepare_linearization_catalog<R: Read + Seek>(pdf: &mut Pdf<R>) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(());
    };
    let root = pdf.get_object_handle(root_ref);
    pdf.resolve(&root)?;
    let extensions = root.try_get_key(b"/Extensions")?;
    if extensions.try_as_dictionary()?.is_none() {
        return Ok(());
    }
    let extensions = if extensions.is_indirect() {
        let direct = extensions.shallow_copy()?;
        root.replace_key(b"/Extensions", direct.clone())?;
        direct
    } else {
        extensions
    };
    if extensions.try_has_key(b"/ADBE")? {
        let mut adbe = extensions.try_get_key(b"/ADBE")?;
        if adbe.is_indirect() {
            adbe.make_direct(false)?;
            extensions.replace_key(b"/ADBE", adbe)?;
            pdf.mark_object_handle_dirty(&root)?;
        }
    }
    Ok(())
}
```

The implementation must mark the root when only `/Extensions` is replaced as
well; the snippet's single dirty call is to be placed behind a `changed` bool
so both replacement cases share it. Keep qpdf's `makeDirect(false)` stream
failure and cycle error behavior; do not use `resolve_object` or a recursive
raw-value walk.

- [x] **Step 2: Move the snapshot before preparation and cover failure cleanup.**

In `write_linearized_for_pdf_writer`, capture
`snapshot_catalog_extensions(pdf)` before calling
`prepare_linearization_catalog(pdf)` and before
`LinearizationPlan::from_pdf_with_writer_options`. Ensure the existing restore
call runs when preparation, planning, or emission returns an error by enclosing
the latter operations in a `Result` closure and restoring before returning its
result.

- [x] **Step 3: Add directization unit tests and run GREEN.**

Test that an indirect dictionary-valued `/Extensions` becomes a direct handle,
that an indirect `/ADBE` becomes direct, that non-dictionary `/Extensions` is
not treated as an extension dictionary, and that an error from an uncloneable
direct stream is propagated. Run:

```bash
cargo test -p flpdf --lib linearization::writer::tests::prepare_linearization_catalog
```

Expected: all new tests pass. Commit:

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "fix: prepare linearization catalog before planning"
```

### Task 3: Remove active raw Catalog resolution

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs:1918-1961`
- Modify: `crates/flpdf/src/linearization/writer.rs:2940-3076`
- Modify: `crates/flpdf/src/linearization/writer.rs:3760-3810`
- Test: `crates/flpdf/src/linearization/writer.rs` existing rejection tests

- [x] **Step 1: Migrate `compute_outline_hint_info`.**

Resolve the Catalog through `get_object_handle` and `Pdf::resolve`, retrieve
`/Outlines` with `try_get_key`, and derive its `ObjectRef` from
`object_ref().or_else(|| as_reference())`. Preserve the existing `unit_of`,
the ObjStm container mapping, and the existing `None` behavior for no outlines.

- [x] **Step 2: Simplify ADBE status to a canonical visible-key check.**

Remove `orphans_indirect_object` and all raw `Object` matching from
`CatalogAdbeStatus`. Resolve the live Catalog, obtain `/Extensions` with
`try_get_key`, return `has_adbe: false` for a non-dictionary value, and use
`try_has_key(b"/ADBE")` for a dictionary. Delete the indirect-subtree
`Unsupported` branch from `write_linearized_impl`; retain the effective-level
injection/strip dispatch.

- [x] **Step 3: Replace tests that pin the rejected behavior.**

Rename the top-level indirect-extension, indirect-ADBE, indirect-extension-level,
and non-dictionary cases to assert successful canonical linearization where
qpdf succeeds. Keep the malformed-array case only if a live qpdf probe shows a
matching result; otherwise remove the flpdf-only rejection assertion and test
the qpdf-observable behavior at the correct writer boundary.

- [x] **Step 4: Run focused GREEN tests.**

```bash
cargo test -p flpdf --lib linearization::writer
cargo test -p flpdf --test linearization_route_contract_tests
cargo test -p flpdf --test cmp_linearize_tests --features qpdf-zlib-compat
cargo test -p flpdf --test linearize_classic_tests
```

Expected: no active production raw-route marker remains and the indirect
extension golden matches qpdf. Commit:

```bash
git add crates/flpdf/src/linearization/writer.rs crates/flpdf/tests/linearization_route_contract_tests.rs crates/flpdf/tests/cmp_linearize_tests.rs
git commit -m "refactor: route linearization catalog through handles"
```

### Task 4: Validate all affected writer and CLI behavior

**Files:**
- Test: `crates/flpdf/tests/linearize_objstm_generate_tests.rs`
- Test: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`
- Test: `crates/flpdf/tests/show_linearization_tests.rs`
- Test: `crates/flpdf-cli/tests/compat_matrix_tests.rs`

- [x] **Step 1: Run qpdf-focused differential and CLI tests.**

```bash
cargo test -p flpdf --test cmp_linearize_tests --features qpdf-zlib-compat
cargo test -p flpdf --test cmp_linearize_objstm_tests --features qpdf-zlib-compat
cargo test -p flpdf --test linearize_objstm_generate_tests
cargo test -p flpdf --test show_linearization_tests
cargo test -p flpdf-cli --test compat_matrix_tests
```

- [x] **Step 2: Run formatting and static quality gates.**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py
python3 scripts/check-qpdf-deviation-markers.py --check
```

- [x] **Step 3: Run the complete workspace and differential gates.**

```bash
cargo test --workspace
scripts/qpdf-tokenizer-diff.sh
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-egzr-3-2-8-18.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/flpdf-egzr-3-2-8-18.lcov
```

Expected: workspace tests pass and patch coverage reports zero uncovered
changed executable lines. Add coverage tests before any claim if the report is
not 100%. Commit any test-only coverage additions separately.

### Task 5: Review, rebase, publish, and record evidence

**Files:**
- Modify: `docs/superpowers/specs/2026-08-26-linearization-catalog-handle-resolution-design.md`
- Modify: `docs/superpowers/plans/2026-08-26-linearization-catalog-handle-resolution.md`
- Modify: Beads issue `flpdf-egzr.3.2.8.18`

- [x] **Step 1: Self-review the exact diff and qpdf correspondence.**

Confirm the production census has no active `resolve_borrowed` or raw
Catalog resolution in `linearization/writer.rs`, the qpdf source citations
match the implementation, and no `canonical_*` rename or unrelated module
cleanup slipped into the diff.

- [x] **Step 2: Rebase and rerun the affected gates.**

```bash
git fetch origin
git rebase origin/main
git status --short --branch
```

Resolve only current conflicts, then rerun the full quality and patch-coverage
commands against the rebased head.

- [ ] **Step 3: Push and create a Draft PR.**

```bash
git push --set-upstream origin feature/flpdf-egzr-3-2-8-18-linearization-catalog
gh pr create --draft --base main --head feature/flpdf-egzr-3-2-8-18-linearization-catalog --title "refactor: migrate linearization Catalog resolution to ObjectHandle" --body-file /tmp/flpdf-egzr-3-2-8-18-pr.md
```

The PR body must state that indirect `/Extensions` is accepted/directized per
qpdf 11.9.0 and that the PR is not to be merged by this session.

- [ ] **Step 4: Wait for every CI check, then mark ready.**

Run `gh pr checks <number>` until Quality, Coverage, patch coverage, Fuzz,
CodeQL, all OS jobs, labels, and release gates are green. Query review APIs and
resolve only findings that remain valid against qpdf source/live behavior.
Only then run `gh pr ready <number>` and read back `state`, `isDraft`, head/base
OIDs, merge state, checks, reviews, and comments. Do not merge.

- [ ] **Step 5: Append final Beads evidence and push it.**

Record the final commit/head, PR URL/state, qpdf source/live evidence, RED→GREEN
tests, focused/full quality gates, patch coverage, review result, and the
deferred `canonical_*` cleanup. Then run:

```bash
bd show flpdf-egzr.3.2.8.18
bd dep cycles
bd dolt push
```

Expected final Beads output includes `Push complete.`; leave the issue
`IN_PROGRESS` because integration/merge is owned by the integration session.
