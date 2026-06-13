# Holder-chain Terminal Normalization (flpdf-k7xx) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (or subagent-driven-development) to implement this plan task-by-task.

**Goal:** Eradicate the three remaining holder-chain (`ref → ref → value`, doubled-indirect) matching gaps (M1/M2/M3) by applying the single existing terminal-normalization helper `resolve_ref_chain` consistently, after relocating it to a neutral low-level module.

**Architecture:** "Single helper" == the existing `resolve_ref_chain` (PR #329 already applied it at 35 page_merge sites). Move it from `outline_dest_remap.rs` (consumer layer) into a new neutral `crate::ref_chain` module so both `name_number_tree` (M1) and `acroform_document_helper` (M2) can import it — `name_number_tree`'s module doc forbids depending on consumers, and an inline second chain-follow in `walk_tree` would defeat the "single helper" intent. Then apply the helper at M1 (`walk_tree` `/Kids` node resolve), M2 (`resolve_array_value` array-carrier resolve), and M3 (`remap_inline_dest_depth` leading page-ref → copy-map match).

**Tech Stack:** Rust (crate `flpdf`), `cargo test`, `scripts/patch-coverage.sh`.

**Scope fence (out of scope — do NOT touch):**
- `remap_inline_name_tree_root`'s `/Kids` remap (`map.get(r).unwrap_or(*r)`) — carrier capture (closure-fold first-hop), per issue.
- object-by-own-ref.
- M2 field-element refs are already terminal-normalized in `source_top_level_field_names`.
- M3 stays map-match normalization only; do not expand the removed-target null-out path.

---

### Task 1: Relocate `resolve_ref_chain` into new `crate::ref_chain` (pure move, no behavior change)

**Files:**
- Create: `crates/flpdf/src/ref_chain.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `mod` decl)
- Modify: `crates/flpdf/src/outline_dest_remap.rs` (remove def + its `resolve_ref_chain`-only constant use; add import)
- Modify: `crates/flpdf/src/page_merge.rs:15`, `crates/flpdf/src/page_extract.rs:47`, `crates/flpdf/src/thread_bead_p.rs:71` (import path)

**Step 1: Create `ref_chain.rs`**

```rust
//! Terminal normalization of indirect-reference chains.
//!
//! A PDF value reached by indirection may be stored behind *more than one*
//! indirect hop (`a 0 R → b 0 R → value`) — a "holder chain". Any code that
//! matches, resolves, or rewrites a reference must follow the chain to its
//! terminal, or a doubled-indirect reference is silently mishandled (a `/Kids`
//! node dropped, an array carrier treated as empty, a copy-map lookup missed).
//! This module owns that one bounded follow-the-chain primitive so every
//! consumer shares a single implementation.

use crate::{Object, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

/// Maximum indirect-reference hops [`resolve_ref_chain`] follows before stopping.
/// A cyclic or maliciously deep chain terminates at this bound rather than
/// looping forever, preserving the no-panic core guarantee on hostile input.
pub(crate) const MAX_REF_CHAIN_DEPTH: usize = 64;

/// Follow a chain of [`Object::Reference`] indirections up to
/// [`MAX_REF_CHAIN_DEPTH`], returning the terminal non-reference object and the
/// last [`ObjectRef`] traversed (for in-place rewrite of, or copy-map matching
/// against, an indirect target). A cyclic / over-deep chain terminates at the
/// bound and yields the last resolved value, so a hostile target cannot loop
/// forever.
pub(crate) fn resolve_ref_chain<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    start: &Object,
) -> Result<(Object, Option<ObjectRef>)> {
    let mut last_ref: Option<ObjectRef> = None;
    let mut cur = start.clone();
    for _ in 0..MAX_REF_CHAIN_DEPTH {
        match cur {
            Object::Reference(r) => {
                last_ref = Some(r);
                cur = pdf.resolve(r)?;
            }
            _ => break,
        }
    }
    Ok((cur, last_ref))
}
```

**Step 2: Register the module in `lib.rs`**

Add (alphabetical position, near `pub(crate) mod rewrite_renumber;` / others):
```rust
pub(crate) mod ref_chain;
```

**Step 3: Remove the old definition in `outline_dest_remap.rs`**

- Delete the `resolve_ref_chain` fn (currently ~lines 737-753) **and** its doc comment.
- Keep `const MAX_DEST_RESOLVE_DEPTH: usize = 64;` (still used by `remap_dest_value_depth` / `dest_page_ref_resolved_depth`).
- Add at the top imports: `use crate::ref_chain::resolve_ref_chain;`

**Step 4: Update consumer import paths**

- `thread_bead_p.rs:71`: `use crate::ref_chain::resolve_ref_chain;`
- `page_extract.rs:47`: split into
  `use crate::outline_dest_remap::dest_page_ref_resolved;`
  `use crate::ref_chain::resolve_ref_chain;`
- `page_merge.rs:15`: same split as page_extract.

**Step 5: Verify build + full suite (behavior unchanged)**

Run: `cargo build -p flpdf && cargo test -p flpdf 2>&1 | rg "test result:|error\["`
Expected: builds clean, all `test result: ok`, 0 failures (pure move).

**Step 6: Commit**

```bash
git add crates/flpdf/src/ref_chain.rs crates/flpdf/src/lib.rs \
  crates/flpdf/src/outline_dest_remap.rs crates/flpdf/src/page_merge.rs \
  crates/flpdf/src/page_extract.rs crates/flpdf/src/thread_bead_p.rs
git commit -m "refactor(flpdf): relocate resolve_ref_chain to neutral crate::ref_chain module (flpdf-k7xx)"
```

---

### Task 2: M1 — `walk_tree` follows holder-chain `/Kids` nodes

**Files:**
- Modify: `crates/flpdf/src/name_number_tree.rs` (imports + `walk_tree` Reference arm ~lines 279-291)
- Test: `crates/flpdf/src/name_number_tree.rs` (`#[cfg(test)] mod tests`)

**Step 1: Write the failing test** (add near `read_name_tree_descends_kids_via_reference`)

```rust
#[test]
fn read_name_tree_descends_kids_via_holder_chain() {
    let mut pdf = empty_pdf();
    // Leaf node at obj 20.
    let mut leaf = Dictionary::new();
    leaf.insert(
        "Names",
        Object::Array(vec![
            Object::String(b"k".to_vec()),
            Object::Reference(ObjectRef::new(99, 0)),
        ]),
    );
    pdf.set_object(ObjectRef::new(20, 0), Object::Dictionary(leaf));
    // Holder: obj 21 is a bare reference to obj 20 (ref → ref → node).
    pdf.set_object(ObjectRef::new(21, 0), Object::Reference(ObjectRef::new(20, 0)));
    // Root /Kids -> [21 0 R]; 21 is a holder chain, not a direct node.
    let mut root = Dictionary::new();
    root.insert("Kids", Object::Array(vec![Object::Reference(ObjectRef::new(21, 0))]));
    let out = read_name_tree(
        &mut pdf,
        Object::Dictionary(root),
        ref_only,
        DEFAULT_MAX_TREE_DEPTH,
    )
    .unwrap();
    assert_eq!(out, vec![(b"k".to_vec(), ObjectRef::new(99, 0))]);
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib read_name_tree_descends_kids_via_holder_chain`
Expected: FAIL — `out` is empty (one-hop resolve yields a `Reference`, `as_dict()` is `None`, node dropped).

**Step 3: Implement** — add import and replace the `Object::Reference(r)` arm of the `match node` block:

Add to imports: `use crate::ref_chain::resolve_ref_chain;`

Replace:
```rust
        Object::Reference(r) => {
            if !visited.insert(r) {
                return Ok(()); // cycle — skip
            }
            match pdf.resolve_borrowed(r)?.as_dict() {
                Some(d) => d.clone(),
                None => return Ok(()), // malformed node — skip
            }
        }
```
with:
```rust
        Object::Reference(r) => {
            if !visited.insert(r) {
                return Ok(()); // cycle — skip
            }
            // A /Kids node ref may be a holder chain (`r → r2 → node dict`);
            // follow it to the terminal so a doubled-indirect kid is descended,
            // not dropped. Holder hops are bounded by resolve_ref_chain's own
            // MAX_REF_CHAIN_DEPTH — a separate axis from the /Kids `visited` /
            // `depth` guards (kept as-is). `into_dict` takes the terminal by
            // value, so no extra clone over the prior `as_dict().clone()`.
            match resolve_ref_chain(pdf, &Object::Reference(r))?.0.into_dict() {
                Some(d) => d,
                None => return Ok(()), // malformed / non-dict node — skip
            }
        }
```

**Step 4: Run to verify it passes + cycle/depth guards intact**

Run: `cargo test -p flpdf --lib name_number_tree`
Expected: new test PASS; `read_name_tree_cycle_terminates`, `read_name_tree_depth_limit_errors`, `read_name_tree_kid_resolving_to_non_dict_is_skipped`, `read_name_tree_descends_kids_via_reference` all still PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/name_number_tree.rs
git commit -m "fix(flpdf): walk_tree follows holder-chain /Kids node refs (flpdf-k7xx M1)"
```

---

### Task 3: M2 — `resolve_array_value` follows holder-chain array carriers

**Files:**
- Modify: `crates/flpdf/src/acroform_document_helper.rs` (imports + `resolve_array_value` ~lines 868-882)
- Test: `crates/flpdf/tests/acroform_document_helper_tests.rs`

**Step 1: Write the failing test** (uses the file's `build_pdf` helper)

```rust
// A `/Fields` array carrier stored as a holder chain (`6 0 R → 7 0 R → [4 0 R]`)
// must still yield its top-level field. A one-hop carrier resolve returns the
// inner `Reference` (not an `Array`) and dropped every field; the chain resolve
// follows to the terminal array.
#[test]
fn top_level_fields_follows_holder_chain_carrier() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /T (f1) /FT /Tx >>"),
            // Holder chain carrier: AcroForm /Fields 6 0 R -> 7 0 R -> [4 0 R].
            (6, "7 0 R"),
            (7, "[4 0 R]"),
            (8, "<< /Fields 6 0 R >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
    let fields = pdf.acroform().top_level_fields().unwrap();
    assert_eq!(fields, vec![ObjectRef::new(4, 0)]);
}
```
(Confirm imports at top of the test file include `Pdf`, `ObjectRef`; add if missing.)

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --test acroform_document_helper_tests top_level_fields_follows_holder_chain_carrier`
Expected: FAIL — `fields` is empty (carrier one-hop returns `Reference(7 0 R)` → `_ => Ok(None)`).

**Step 3: Implement** — add import and replace the `Reference` arm of `resolve_array_value`:

Add import: `use crate::ref_chain::resolve_ref_chain;`

Replace:
```rust
        Some(Object::Reference(object_ref)) => match pdf.resolve_borrowed(object_ref)? {
            Object::Array(values) => Ok(Some(values.clone())),
            Object::Null => Ok(None),
            _ => Ok(None),
        },
```
with:
```rust
        Some(value @ Object::Reference(_)) => {
            // The array carrier itself may be a holder chain (`/Fields 20 0 R →
            // 21 0 R → [..]`); follow it to the terminal so a doubled-indirect
            // carrier yields its array instead of being dropped as a non-array.
            // The terminal is returned by value, so the array moves out without
            // the prior `.clone()`.
            match resolve_ref_chain(pdf, &value)?.0 {
                Object::Array(values) => Ok(Some(values)),
                _ => Ok(None), // Null or non-array terminal
            }
        }
```
(Keep the direct `Some(Object::Array(values)) => Ok(Some(values))` fast path unchanged.)

**Step 4: Run to verify it passes + blast-radius check**

Run: `cargo test -p flpdf --test acroform_document_helper_tests && cargo test -p flpdf 2>&1 | rg "test result:|error\["`
Expected: new test PASS; existing `malformed_fields` / `indirect_malformed_fields` style tests still PASS (single-ref-to-non-array still yields `None`). 0 failures workspace-wide.

**Step 5: Commit**

```bash
git add crates/flpdf/src/acroform_document_helper.rs \
  crates/flpdf/tests/acroform_document_helper_tests.rs
git commit -m "fix(flpdf): resolve_array_value follows holder-chain array carriers (flpdf-k7xx M2)"
```

---

### Task 4: M3 — `remap_inline_dest_depth` terminal-normalizes the leading page ref

**Files:**
- Modify: `crates/flpdf/src/page_merge.rs` (`remap_inline_dest_depth` Array arm ~lines 921-929)
- Test: `crates/flpdf/tests/page_merge_tests.rs`

**Step 1: Write the failing test** (mirror `merge_inline_legacy_dests_non_array_remapped`)

```rust
// M3: an inline (on-catalog) dest array whose LEADING page ref is itself a
// holder chain (`30 0 R → 3 0 R`, the page) must remap to the copied page. The
// copy map keys pages by their terminal ref, so a one-hop match on the holder
// `30 0 R` misses and emits the uncopied source holder (resolving to Null);
// terminal normalization matches the page and remaps to the new page0.
#[test]
fn merge_inline_dest_holder_chain_leading_ref_remapped() {
    let pdf = build_pdf(
        &[
            (
                1,
                "<< /Type /Catalog /Pages 2 0 R \
                 /Dests << /d_holder << /D [30 0 R /Fit] >> >> >>",
            ),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (30, "3 0 R"), // holder: 30 0 R -> 3 0 R (page0)
        ],
        1,
    );
    let mut a = Pdf::open_mem_owned(pdf).unwrap();
    let mut inputs = [MergeInput { source: &mut a, pages: vec![0] }];
    let mut doc = merge_documents(&mut inputs).unwrap();

    let refs = pages::page_refs(&mut doc).unwrap();
    assert_eq!(refs.len(), 1);
    let page0 = refs[0];
    let cat = catalog_dict(&mut doc);
    let legacy = match cat.get("Dests") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected inline legacy /Dests, got {other:?}"),
    };
    let d_holder = match legacy.get("d_holder") {
        Some(Object::Dictionary(d)) => d.clone(),
        other => panic!("expected /d_holder dict dest, got {other:?}"),
    };
    let arr = match d_holder.get("D") {
        Some(Object::Array(a)) => a.clone(),
        other => panic!("expected /d_holder /D array, got {other:?}"),
    };
    let (first, is_null) = dest_array_first(&mut doc, &arr);
    assert_eq!(first, page0, "holder-chain leading ref remaps to new page0");
    assert!(!is_null, "surviving holder-chain dest must not resolve to null");
}
```
(Confirm `MergeInput`, `merge_documents`, `catalog_dict`, `dest_array_first`, `pages` are already imported in the test file — they are used by the adjacent tests.)

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --test page_merge_tests merge_inline_dest_holder_chain_leading_ref_remapped`
Expected: FAIL — first elem is not `page0` (map miss on holder `30`, stays source ref → resolves Null).

**Step 3: Implement** — replace the `Object::Array(mut arr)` arm of `remap_inline_dest_depth`:

```rust
        Object::Array(mut arr) => {
            // The leading page ref may be a holder chain (`r → r2 → page`); the
            // copy map keys pages by their TERMINAL ref, so normalize before
            // matching — otherwise a doubled-indirect dest ref misses the map and
            // is emitted as the (uncopied) source holder ref. A direct page ref
            // normalizes to itself, so the common case is unchanged.
            let first_ref = match arr.first() {
                Some(Object::Reference(r)) => Some(*r),
                _ => None,
            };
            if let Some(r) = first_ref {
                if let Some(terminal) = resolve_ref_chain(source, &Object::Reference(r))?.1 {
                    if let Some(&new_ref) = map.get(&terminal) {
                        arr[0] = Object::Reference(new_ref);
                    }
                }
            }
            Ok(Object::Array(arr))
        }
```
(`resolve_ref_chain` already imported in page_merge.rs.)

**Step 4: Run to verify it passes + adjacent dest tests intact**

Run: `cargo test -p flpdf --test page_merge_tests`
Expected: new test PASS; `merge_inline_legacy_dests_non_array_remapped`, `merge_inline_open_action_next_chain_remapped`, and all other inline-dest tests still PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_merge.rs crates/flpdf/tests/page_merge_tests.rs
git commit -m "fix(flpdf): terminal-normalize inline dest leading page ref before copy-map match (flpdf-k7xx M3)"
```

---

### Task 5: Quality gates + changed-line coverage

**Step 1: Full workspace test**

Run: `cargo test --workspace 2>&1 | rg "test result:|error\[|FAILED"`
Expected: all `ok`, 0 failures.

**Step 2: Format + lint**

Run: `cargo fmt --all && cargo fmt --all --check && cargo clippy -p flpdf --all-targets -- -D warnings`
Expected: no diff, no clippy errors. (Commit any fmt change.)

**Step 3: Changed-line coverage gate (flpdf must be 100%)**

Run: `scripts/patch-coverage.sh --base main`
Expected: exit 0, no uncovered changed lines in `flpdf`. If any line is uncovered, add a test or justify with `// cov:ignore: <reason>` (note in PR).

**Step 4: Doc/public-surface sanity**

`ref_chain.rs` items are `pub(crate)` (not docs.rs-published), but keep doc clean: no beads issue IDs / internal jargon in `//!` / `///` per `.claude/rules/pdf-rust-doc-review-patterns.md`. Verify:
Run: `grep -rnE '(///|//!).*(flpdf-[0-9a-z.]+|epic|follow-up|TODO|FIXME)' crates/flpdf/src/ref_chain.rs` → expect no output.

**Step 5: Final verification before PR**

REQUIRED SUB-SKILL: superpowers:verification-before-completion — confirm test output, coverage exit code, and clean `git status` with real command output before claiming done.
