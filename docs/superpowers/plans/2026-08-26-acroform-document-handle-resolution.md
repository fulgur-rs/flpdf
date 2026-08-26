# AcroForm Document Handle Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the active AcroForm document helper's raw Catalog/field resolution and preserve qpdf's live ObjectHandle identity through field traversal and `/AcroForm/DA` mutation.

**Architecture:** Convert the active AcroForm helper's dictionary, array, and inherited-value walkers to `ObjectHandle` values and use `Pdf::resolve` plus typed handle accessors. Change the public field-info value members to handles so the migration does not add a materialization bridge. Keep the separate raw foreign graph-copy helper and existing documented `resolve_to_terminal` compensation outside this slice.

**Tech Stack:** Rust workspace, `ObjectHandle`, qpdf 11.9.0 pinned source/live probe, Cargo tests, strict rustdoc, all-features Clippy, qpdf module/deviation checks, and `cargo llvm-cov`.

---

### Task 1: Add the route contract and observe RED

**Files:**
- Modify: `crates/flpdf/tests/legacy_route_cutover_tests.rs`
- Test: `crates/flpdf/tests/legacy_route_cutover_tests.rs`

- [ ] **Step 1: Add a source contract for the active AcroForm boundaries.**

Add a test that extracts the `acroform_dict` and `resolve_dict` function
sections and rejects the raw resolver/materialization markers while requiring
the live-handle markers. Also require the field-info value members to carry
`ObjectHandle` rather than `Object`:

```rust
#[test]
fn acroform_active_resolution_uses_live_handle_route() {
    let source = include_str!("../src/acroform_document_helper.rs");
    for marker in ["fn acroform_dict", "fn resolve_dict"] {
        let section = source
            .split_once(marker)
            .expect("AcroForm resolver marker must remain present")
            .1
            .split_once("\n    fn ")
            .expect("AcroForm resolver must be followed by another helper")
            .0;
        for legacy in [
            "resolve_borrowed",
            "resolve_object",
            "Object::Reference",
            "Object::Dictionary",
            "dict.clone()",
        ] {
            assert!(
                !section.contains(legacy),
                "{marker} still contains raw resolution marker {legacy:?}"
            );
        }
        let canonical = if marker == "fn acroform_dict" {
            ["try_get_key", "resolve_to_terminal", "try_as_dictionary"]
        } else {
            ["get_object_handle", "resolve(", "try_as_dictionary"]
        };
        for canonical in canonical {
            assert!(
                section.contains(canonical),
                "{marker} must contain canonical handle marker {canonical:?}"
            );
        }
    }

    for field in ["value", "default_value", "default_appearance"] {
        let marker = format!("pub {field}: Option<ObjectHandle>");
        assert!(
            source.contains(&marker),
            "AcroFormFieldInfo::{field} must preserve live ObjectHandle values"
        );
    }
}
```

- [ ] **Step 2: Run only the new contract test and verify RED.**

Run:

```bash
cargo test -p flpdf --test legacy_route_cutover_tests acroform_active_resolution_uses_live_handle_route
```

Expected: FAIL on the current `resolve_borrowed`/raw `Object` markers, not a
compilation error or a typo in the source slice.

### Task 2: Convert the Catalog and AcroForm dictionary boundaries

**Files:**
- Modify: `crates/flpdf/src/acroform_document_helper.rs`
- Test: `crates/flpdf/tests/acroform_document_helper_tests.rs`

- [ ] **Step 1: Return a live AcroForm handle.**

Change `acroform_dict` to return `Result<Option<ObjectHandle>>`. Resolve the
Catalog handle, fetch `/AcroForm` with `try_get_key`, resolve the returned
handle through the existing canonical one-hop/terminal helper, and return it
only when it is a dictionary. A missing/null/non-dictionary AcroForm returns
`None`, matching `QPDFAcroFormDocumentHelper::analyze`.

- [ ] **Step 2: Return a live dictionary handle for known object refs.**

Change `resolve_dict` and `resolve_field_dict` to return `ObjectHandle`.
Construct the canonical handle with `get_object_handle`, call `Pdf::resolve`,
and use `try_as_dictionary` for the type check. Preserve
`"{label} object {object_ref} is not a dictionary"` for the error branch.

- [ ] **Step 3: Migrate `acroform_ref` and `ensure_acroform_ref`.**

Read `/AcroForm` from the live Catalog handle and return its object reference
without cloning a raw dictionary. When creating an AcroForm, make a fresh
indirect handle for the existing direct dictionary or for
`<< /Fields [] >>`, replace the Catalog key with that handle, mark the Catalog
dirty, and invalidate the association cache exactly as the current mutation
does.

- [ ] **Step 4: Migrate `/AcroForm/DA` mutation.**

Replace the raw `Dictionary::insert` plus `Pdf::set_object` sequence in
`set_default_appearance` with:

```rust
let acroform = self.resolve_dict(acroform_ref, "AcroForm")?;
acroform.replace_key(b"/DA", ObjectHandle::string(appearance))?;
self.pdf.mark_object_handle_dirty(&acroform)?;
Ok(())
```

- [ ] **Step 5: Run the AcroForm focused suite.**

Run:

```bash
cargo test -p flpdf --lib acroform_document_helper
cargo test -p flpdf --test acroform_document_helper_tests
```

Expected: the suite compiles after the handle return types are propagated and
all existing field traversal, malformed input, and mutation tests pass.

### Task 3: Migrate field arrays, inherited metadata, and the public snapshot

**Files:**
- Modify: `crates/flpdf/src/acroform_document_helper.rs`
- Modify: `crates/flpdf/examples/list_form_fields.rs`
- Modify: `crates/flpdf/tests/helper_api_tests.rs`
- Modify: `crates/flpdf/tests/acroform_document_helper_tests.rs`

- [ ] **Step 1: Convert array and field traversal helpers to handles.**

Change `resolve_array_value` to accept `Option<ObjectHandle>` and return
`Option<Vec<ObjectHandle>>`, resolving the carrier through the existing
canonical handle path. Update `fields`, `top_level_fields`, `has_fields_array`,
`walk_field_tree_rec`, and `walk_field_info_tree` to use `object_ref()` and
handle typed accessors while preserving order, duplicate suppression, depth
limits, and direct-child filtering.

- [ ] **Step 2: Convert inherited metadata state to handles.**

Change `FieldInheritance.value`, `default_value`, and `default_appearance` to
`Option<ObjectHandle>`. Replace `deref_leaf`, `inherited_object`,
`inherited_name`, and `inherited_integer` with handle-based versions that
return absent for resolved nulls and preserve the current terminal-chain
behavior. Change `is_pure_widget_annotation` and field-info construction to
use `try_has_key`, `try_as_name`, `try_as_string`, and `try_as_integer` on live
handles.

- [ ] **Step 3: Change `AcroFormFieldInfo` value fields and consumers.**

Use `Option<ObjectHandle>` for `value`, `default_value`, and
`default_appearance`. Update example/test assertions to call `as_string()` or
other typed handle accessors. Do not call `materialize`, `unparse`, or
`resolve_object` to preserve the former snapshot shape.

- [ ] **Step 4: Run the route contract and all AcroForm behavior tests.**

Run:

```bash
cargo test -p flpdf --test legacy_route_cutover_tests acroform_active_resolution_uses_live_handle_route
cargo test -p flpdf --test acroform_document_helper_tests
cargo test -p flpdf --test helper_api_tests acroform_helper_field_infos_match_manual_and_retain_indirect_handle
cargo test -p flpdf --lib acroform_document_helper
```

Expected: the source contract is GREEN and direct/indirect/malformed field
behavior remains covered.

### Task 4: Verify qpdf behavior and the affected workspace

- [ ] **Step 1: Run the live qpdf AcroForm probes and focused differential tests.**

Run the pinned qpdf JSON probes for direct and indirect non-dictionary
`/AcroForm`, then run the repository's AcroForm qpdf parity targets:

```bash
qpdf --json --json-key=acroform /tmp/qpdf-acroform-inline-nondict-probe.pdf -
qpdf --json --json-key=acroform /tmp/qpdf-acroform-indirect-nondict-probe.pdf -
cargo test -p flpdf --test remove_restrictions_qpdf_parity
cargo test -p flpdf --test page_annotation_route_cutover_tests
```

- [ ] **Step 2: Confirm the scoped raw-route census.**

Run:

```bash
rg -n 'resolve_borrowed|resolve_object|materialize|set_object\(|Object::Reference|Object::Dictionary' crates/flpdf/src/acroform_document_helper.rs
```

The active Catalog/AcroForm/field-info functions must contain no raw resolver
or materialization route. Raw graph-copy helpers used by the separately owned
overlay legacy bridge may remain and must be named in the implementation note.

- [ ] **Step 3: Run formatting, docs, Clippy, and workspace tests.**

```bash
cargo fmt --all -- --check
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
python3 scripts/qpdf-module-docs.py --check
python3 -m unittest scripts/tests/test_qpdf_deviation_markers.py
python3 scripts/check-qpdf-deviation-markers.py --check
```

- [ ] **Step 4: Run fresh parent-relative patch coverage.**

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/acroform-document-handle.lcov
scripts/patch-coverage.sh --base origin/main --lcov /tmp/acroform-document-handle.lcov
```

Expected: `flpdf` reports zero uncovered changed executable lines.

### Task 5: Review, publish, and persist evidence

- [ ] **Step 1: Re-read the final diff and request technical review.**

Review only `origin/main...HEAD`; verify qpdf citations, raw-route scope, and
that no new compatibility adapter or prefix-only rename was introduced.

- [ ] **Step 2: Rebase and publish a Draft PR without merging.**

Fetch/rebase the latest `origin/main`, rerun affected tests and coverage, push
`feature/flpdf-egzr-3-2-8-16-acroform-handle`, and create a Draft PR. Mark it
ready only after every CI check, including `codecov/patch`, is green. Do not
merge it.

- [ ] **Step 3: Append evidence to Beads and push Dolt.**

Record the worktree, commits, qpdf source/live probes, RED→GREEN result, gates,
PR state, and the remaining parent scope on `flpdf-egzr.3.2.8.16`. Then run
`bd dep cycles` and `bd dolt push`, confirming `Push complete.`.
