# OBJR /Obj-survived annotation `/P` drop Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Match qpdf `--pages` behaviour by dropping the dangling `/P` back-reference on an annotation kept alive only through a structure-tree object reference (`/Type /OBJR`) `/Obj`, so the now-unreferenced removed page is garbage-collected (issue flpdf-u2kh).

**Architecture:** Reuse the single struct-tree walk in `struct_tree_pg.rs` to *collect* the OBJR `/Obj` target refs it already passes over (changing the two public entry points to return `Vec<ObjectRef>` — pre-1.0, signature change is fine). A new sibling module `objr_obj_annot_p.rs` consumes those refs and applies the same remap-or-drop on each target's `/P` that `thread_bead_p`/`struct_tree_pg` apply on `/P`/`/Pg`. Wired into the CLI extraction pipeline as a new step between the existing drop passes and the prune.

**Tech Stack:** Rust, `flpdf` core crate, `flpdf-cli`, qpdf 11.9.0 as oracle truth source, `cargo test` / `cargo llvm-cov` / `scripts/patch-coverage.sh`.

**Empirical truth (already observed, qpdf 11.9.0):** For a removed page reachable only via an OBJR `/Obj` annotation's `/P`, qpdf drops the annot `/P`, keeps the annot and the OBJR `/Obj`, and GCs the page (absent, not `null`). flpdf currently keeps the `/P`, leaving an orphan `/Type /Page`. The OBJR's own dangling `/Pg` is already dropped by the existing `struct_tree_pg` pass.

---

### Task 1: New module `objr_obj_annot_p.rs` — the `/P` remap-or-drop pass

Self-contained: takes the OBJR `/Obj` target refs as a parameter, so it has no dependency on the Task 2 collection change yet (unit-tested with manually-supplied refs).

**Files:**
- Create: `crates/flpdf/src/objr_obj_annot_p.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `mod objr_obj_annot_p;` + `pub use`)

**Step 1: Create the module with the implementation and doc.**

Create `crates/flpdf/src/objr_obj_annot_p.rs`:

```rust
//! Annotation `/P` reference drop for annotations kept alive only through a
//! structure-tree object reference (`/Type /OBJR`) `/Obj`, after page
//! extraction.
//!
//! After [`crate::page_tree_rebuild::rebuild_page_tree`] has rebuilt the page
//! tree for a subset extraction, an annotation on a removed page is normally
//! garbage-collected with that page. But when a structure-tree object reference
//! (`/Type /OBJR`, ISO 32000-2 §14.7.4.4) keeps the annotation alive through its
//! `/Obj`, the annotation survives — and if its `/P` (the page the annotation is
//! on, §12.5.2) still points at the removed page, that back-reference keeps the
//! page alive too, leaving an orphan `/Type /Page` in the output.
//!
//! This pass updates each such annotation's `/P` to match qpdf's `--pages`
//! behaviour:
//!
//! - A `/P` pointing at a **surviving** page keeps the entry, remapped to the
//!   page's new [`ObjectRef`] when the rebuild changed it.
//! - A `/P` pointing at a **removed** page has the `/P` key **dropped**. The
//!   annotation itself (and the OBJR `/Obj` reaching it) is retained; the
//!   now-unreferenced page is garbage-collected by the subsequent subset sweep
//!   ([`crate::subset_prune`]) and is absent from the output.
//!
//! This is the structural-reference *drop* family, alongside the structure-tree
//! `/Pg` handling ([`crate::struct_tree_pg`]) and the article-thread bead `/P`
//! handling ([`crate::thread_bead_p`]): the reference is removed rather than
//! replaced with `null`.
//!
//! # qpdf 11.9.0 observed behaviour (truth source `/usr/bin/qpdf`)
//!
//! For `qpdf in.pdf --pages in.pdf 1,3 -- out.pdf` over a document whose page 2
//! is referenced only by an OBJR `/Obj` annotation's `/P`, qpdf drops that
//! annotation's `/P` and the removed page is absent from the output (not emitted
//! as `null`). The annotation survives via the OBJR `/Obj`, which qpdf keeps.
//!
//! # Scope
//!
//! Only the `/P` of annotations reached through a structure-tree OBJR `/Obj` is
//! handled here. Out of scope:
//!
//! - Annotations on surviving pages (their `/P` is the page they live on, kept
//!   by the writer's reference remap).
//! - AcroForm widget annotations, handled by the field/widget prune.
//! - A direct (inline) `/Obj` dictionary: `/Obj` is by spec an indirect
//!   reference, so an inline object is malformed and left unchanged.
//! - An OBJR `/Obj` target without a `/P`, or whose `/P` is not a reference.

use crate::page_tree_rebuild::RebuildResult;
use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

/// Drop dangling `/P` references on annotations kept alive through a
/// structure-tree OBJR `/Obj`, after a page-tree rebuild (qpdf `--pages`
/// parity).
///
/// `result` is the [`RebuildResult`] returned by
/// [`crate::page_tree_rebuild::rebuild_page_tree`]; its `ref_map` encodes the
/// old → new page mapping (a page absent from the map was removed; a page
/// present maps to `ref_map[old][0]`). `objr_obj_targets` are the OBJR `/Obj`
/// references collected during the structure-tree walk
/// ([`crate::struct_tree_pg::drop_struct_elem_dangling_pg`]).
///
/// Each target is resolved (reference-to-reference chains normalized to their
/// terminal ref and deduplicated by a visited set). When the target is a
/// dictionary whose `/P` is a reference to a removed page, the `/P` key is
/// dropped so the page is garbage-collected by the subsequent subset sweep
/// ([`crate::subset_prune::prune_after_subset`]); when `/P` points at a
/// surviving page it is remapped to the page's new ref. A target with no `/P`,
/// or a `/P` that is not a reference, is left unchanged. The function mutates
/// `pdf` in place and succeeds silently when `objr_obj_targets` is empty.
///
/// # Errors
///
/// Any error propagated from [`Pdf::resolve`] / [`Pdf::resolve_borrowed`] while
/// resolving a target annotation or its `/P` chain.
pub fn drop_objr_obj_annot_dangling_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    result: &RebuildResult,
    objr_obj_targets: &[ObjectRef],
) -> Result<()> {
    let surviving: BTreeMap<ObjectRef, ObjectRef> = result
        .ref_map
        .iter()
        .filter_map(|(&old, new_refs)| new_refs.first().map(|&new| (old, new)))
        .collect();

    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    for &start in objr_obj_targets {
        // Normalize a reference chain so the visited key and the write-back
        // target are the terminal annotation ref, never an intermediate holder.
        let (concrete, terminal) = resolve_ref_chain(pdf, &Object::Reference(start))?;
        let annot_ref = terminal.unwrap_or(start);
        if !visited.insert(annot_ref) {
            continue;
        }
        let Some(mut annot) = concrete.into_dict() else {
            continue;
        };
        if remap_or_drop_annot_p(pdf, &mut annot, &surviving)? {
            pdf.set_object(annot_ref, Object::Dictionary(annot));
        }
    }
    Ok(())
}

/// Remap-or-drop the `/P` of one annotation dictionary. Returns whether the
/// dictionary changed.
///
/// `/P` is by spec an indirect reference to the page the annotation is on; any
/// other form is malformed and left unchanged. A surviving target is remapped to
/// its new ref (an identity remap in a single-document rebuild); a removed target
/// has the key dropped so the page is garbage-collected.
fn remap_or_drop_annot_p<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    annot: &mut Dictionary,
    surviving: &BTreeMap<ObjectRef, ObjectRef>,
) -> Result<bool> {
    let p_ref = match annot.get("P") {
        Some(Object::Reference(r)) => *r,
        _ => return Ok(false),
    };
    // Normalize a possible reference-to-reference chain to the terminal page ref.
    let (_, terminal) = resolve_ref_chain(pdf, &Object::Reference(p_ref))?;
    let page_ref = terminal.unwrap_or(p_ref);
    match surviving.get(&page_ref) {
        Some(&new) => {
            if new != page_ref {
                annot.insert("P", Object::Reference(new));
                return Ok(true);
            }
            Ok(false)
        }
        None => {
            annot.remove("P");
            Ok(true)
        }
    }
}
```

**Step 2: Wire the module into the crate.**

In `crates/flpdf/src/lib.rs`, add the module declaration next to the other `mod` lines (keep alphabetical / existing grouping with `struct_tree_pg` and `thread_bead_p`):

```rust
mod objr_obj_annot_p;
```

and the re-export next to `pub use thread_bead_p::drop_thread_bead_dangling_p;`:

```rust
pub use objr_obj_annot_p::drop_objr_obj_annot_dangling_p;
```

Verify the exact existing lines first:
Run: `grep -n "mod thread_bead_p\|mod struct_tree_pg\|drop_thread_bead_dangling_p" crates/flpdf/src/lib.rs`

**Step 3: Add the failing unit tests** (append a `#[cfg(test)] mod tests` to `objr_obj_annot_p.rs`).

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
    use std::collections::BTreeMap;
    use std::io::Cursor;

    /// Serialize `objs` (object number → body) into a classic-xref PDF with
    /// `/Root 1 0 R`.
    fn build_pdf(objs: &BTreeMap<u32, String>) -> Vec<u8> {
        let mut raw: Vec<u8> = b"%PDF-1.5\n".to_vec();
        let mut offs: BTreeMap<u32, usize> = BTreeMap::new();
        for (n, body) in objs {
            offs.insert(*n, raw.len());
            raw.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        let max_num = *objs.keys().max().unwrap();
        let xref_pos = raw.len();
        raw.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", max_num + 1).as_bytes());
        for i in 1..=max_num {
            if let Some(&off) = offs.get(&i) {
                raw.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            } else {
                raw.extend_from_slice(b"0000000000 65535 f \n");
            }
        }
        raw.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_pos}\n%%EOF\n",
                max_num + 1
            )
            .as_bytes(),
        );
        raw
    }

    fn open(objs: &BTreeMap<u32, String>) -> Pdf<Cursor<Vec<u8>>> {
        Pdf::open(Cursor::new(build_pdf(objs))).expect("open fixture")
    }

    /// Base: catalog (1), pages root (2) /Kids [3 4 5], three pages (3,4,5).
    /// The annotation under test is object 30.
    fn base() -> BTreeMap<u32, String> {
        let mut objs: BTreeMap<u32, String> = BTreeMap::new();
        objs.insert(1, "<< /Type /Catalog /Pages 2 0 R >>".into());
        objs.insert(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>".into());
        for n in 3..=5 {
            objs.insert(n, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".into());
        }
        objs
    }

    /// `RebuildResult` keeping pages 3 and 5 (page 4 removed), identity refs.
    fn keep_3_and_5() -> RebuildResult {
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(3, 0)]);
        ref_map.insert(ObjectRef::new(5, 0), vec![ObjectRef::new(5, 0)]);
        RebuildResult { new_kids: vec![ObjectRef::new(3, 0), ObjectRef::new(5, 0)], ref_map }
    }

    fn annot(pdf: &mut Pdf<Cursor<Vec<u8>>>, num: u32) -> Dictionary {
        match pdf.resolve(ObjectRef::new(num, 0)).expect("resolve annot") {
            Object::Dictionary(d) => d,
            other => panic!("object {num} is not a dictionary: {other:?}"),
        }
    }

    #[test]
    fn dangling_p_to_removed_page_dropped() {
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 4 0 R /Rect [0 0 10 10] >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        assert!(annot(&mut pdf, 30).get("P").is_none(), "removed-page /P must be dropped");
    }

    #[test]
    fn p_to_surviving_page_kept() {
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 3 0 R /Rect [0 0 10 10] >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Reference(r)) if r.number == 3),
            "surviving-page /P must be kept",
        );
    }

    #[test]
    fn p_to_surviving_page_remapped_to_new_ref() {
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 3 0 R /Rect [0 0 10 10] >>".into());
        let mut pdf = open(&objs);
        // Page 3 survives under a new ref (7 0 R), as a duplicate selection can produce.
        let mut ref_map: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
        ref_map.insert(ObjectRef::new(3, 0), vec![ObjectRef::new(7, 0)]);
        let result = RebuildResult { new_kids: vec![ObjectRef::new(7, 0)], ref_map };
        drop_objr_obj_annot_dangling_p(&mut pdf, &result, &[ObjectRef::new(30, 0)]).expect("drop");
        assert!(
            matches!(annot(&mut pdf, 30).get("P"), Some(Object::Reference(r)) if r.number == 7),
            "surviving-page /P must be remapped to the new ref",
        );
    }

    #[test]
    fn target_without_p_left_unchanged() {
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(30, 0)])
            .expect("drop");
        let a = annot(&mut pdf, 30);
        assert!(a.get("P").is_none() && a.get("Subtype").is_some(), "non-/P annot untouched");
    }

    #[test]
    fn empty_targets_is_noop() {
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 4 0 R >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[]).expect("noop");
        assert!(annot(&mut pdf, 30).get("P").is_some(), "no targets ⇒ no change");
    }

    #[test]
    fn chained_obj_and_p_normalized() {
        // Target is reached via a reference chain (40 → 30), and /P is itself a
        // chain (50 → 4 removed). Both terminals must be resolved.
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 50 0 R /Rect [0 0 10 10] >>".into());
        objs.insert(40, "30 0 R".into());
        objs.insert(50, "4 0 R".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(&mut pdf, &keep_3_and_5(), &[ObjectRef::new(40, 0)])
            .expect("drop");
        assert!(annot(&mut pdf, 30).get("P").is_none(), "chained /P to removed page must drop");
    }

    #[test]
    fn shared_target_deduped() {
        // Same annot ref supplied twice: visited dedup must not double-process.
        let mut objs = base();
        objs.insert(30, "<< /Type /Annot /Subtype /Text /P 3 0 R /Rect [0 0 10 10] >>".into());
        let mut pdf = open(&objs);
        drop_objr_obj_annot_dangling_p(
            &mut pdf,
            &keep_3_and_5(),
            &[ObjectRef::new(30, 0), ObjectRef::new(30, 0)],
        )
        .expect("drop");
        assert!(annot(&mut pdf, 30).get("P").is_some(), "surviving /P kept; dedup safe");
    }
}
```

**Step 4: Run the tests.**

Run: `cargo test -p flpdf --lib objr_obj_annot_p`
Expected: all 7 tests PASS.

**Step 5: Commit.**

```bash
git add crates/flpdf/src/objr_obj_annot_p.rs crates/flpdf/src/lib.rs
git commit -m "feat(flpdf): drop OBJR /Obj-survived annotation dangling /P (flpdf-u2kh)"
```

---

### Task 2: Collect OBJR `/Obj` targets during the struct-tree walk

Change `struct_tree_pg`'s two public entry points to return the OBJR `/Obj` target refs gathered during the existing single walk (no second traversal).

**Files:**
- Modify: `crates/flpdf/src/struct_tree_pg.rs`

**Step 1: Add the failing collection assertion to an existing unit test.**

Extend `mcr_and_objr_dangling_pg_dropped` (it already has `21 0 R = << /Type /OBJR /Pg 4 0 R /Obj 5 0 R >>`). Change its call site to capture the return value and assert the `/Obj` target (object 5) was collected:

```rust
        let targets =
            drop_struct_elem_dangling_pg(&mut pdf, &keep_3_and_5()).expect("pg drop");
        assert!(
            targets.contains(&ObjectRef::new(5, 0)),
            "OBJR /Obj target (object 5) must be collected, got {targets:?}"
        );
```

Run: `cargo test -p flpdf --lib struct_tree_pg::tests::mcr_and_objr_dangling_pg_dropped`
Expected: FAIL to compile (`drop_struct_elem_dangling_pg` returns `()`, `.contains` not found) — confirms the change is needed.

**Step 2: Change the return type and thread a collection vec.**

In `struct_tree_pg.rs`:

1. `drop_struct_elem_dangling_pg` → return type `Result<Vec<ObjectRef>>`; body just forwards:
   ```rust
   pub fn drop_struct_elem_dangling_pg<R: Read + Seek>(
       pdf: &mut Pdf<R>,
       result: &RebuildResult,
   ) -> Result<Vec<ObjectRef>> {
       drop_struct_elem_dangling_pg_with_max_depth(pdf, result, DEFAULT_MAX_STRUCT_TREE_DEPTH)
   }
   ```

2. `drop_struct_elem_dangling_pg_with_max_depth` → return type `Result<Vec<ObjectRef>>`. Add `let mut objr_obj_targets: Vec<ObjectRef> = Vec::new();` after the `surviving` map. Thread `&mut objr_obj_targets` into every `walk_kids` call (both the `Some(Object::Reference(root_ref))` and `Some(Object::Dictionary(mut root))` arms). Return `Ok(objr_obj_targets)` at the end (replace the final `Ok(())`).

3. `walk_kids`, `walk_kid_ref`, `process_elem_dict` — add a trailing parameter
   `objr_obj_targets: &mut Vec<ObjectRef>` and pass it through every recursive
   call (mirroring how `visited` is threaded).

4. In `process_elem_dict`, **after** the `/Pg` remap-or-drop block and before the
   `/K` recursion, collect the OBJR `/Obj` target:
   ```rust
   // Collect an object-reference (/Type /OBJR) kid's /Obj target. The object
   // reached through /Obj (an annotation) survives the prune via this
   // reference; a separate pass (objr_obj_annot_p) drops its dangling /P
   // back-reference to a removed page. /Obj is by spec an indirect reference;
   // normalize a reference chain to its terminal ref. A non-reference /Obj is
   // malformed and ignored. Only OBJR dicts carry /Obj, so no /Type check is
   // needed.
   if let Some(Object::Reference(obj)) = dict.get("Obj") {
       let obj = *obj;
       let (_, terminal) = resolve_ref_chain(pdf, &Object::Reference(obj))?;
       objr_obj_targets.push(terminal.unwrap_or(obj));
   }
   ```

5. Add the import: `use crate::ref_chain::resolve_ref_chain;` (top of file).

**Step 3: Capture the return value at the one production call site.**

In `crates/flpdf-cli/src/main.rs:3252`, change:
```rust
    drop_struct_elem_dangling_pg(&mut pdf, &result)?;
```
to:
```rust
    let objr_obj_targets = drop_struct_elem_dangling_pg(&mut pdf, &result)?;
```
(The variable is consumed in Task 3. Until then it is unused — that is fine
because Task 2 and Task 3 land in one push; if running Task 2 alone, prefix
`_objr_obj_targets` to avoid the warning, then rename in Task 3.)

The other call sites (`.expect(...)` / `.unwrap_err()` in unit + integration
tests) compile unchanged: they discard the returned `Vec`.

**Step 4: Run the tests.**

Run: `cargo test -p flpdf --lib struct_tree_pg`
Expected: all PASS (12 existing + the extended assertion).
Run: `cargo build -p flpdf-cli`
Expected: builds (with `_objr_obj_targets` unused-but-prefixed, or proceed straight to Task 3).

**Step 5: Commit.**

```bash
git add crates/flpdf/src/struct_tree_pg.rs crates/flpdf-cli/src/main.rs
git commit -m "feat(flpdf): collect OBJR /Obj targets in struct-tree walk (flpdf-u2kh)"
```

---

### Task 3: Wire the new pass into the CLI extraction pipeline

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (imports near line 9–10; pipeline near line 3258–3261)

**Step 1: Add the import.**

In the `use flpdf::{...}` block (around line 9–10), add
`objr_obj_annot_p::drop_objr_obj_annot_dangling_p,` alongside
`thread_bead_p::drop_thread_bead_dangling_p,`. Confirm exact form first:
Run: `grep -n "drop_thread_bead_dangling_p\|drop_struct_elem_dangling_pg" crates/flpdf-cli/src/main.rs`

**Step 2: Insert the pipeline step.**

After the `drop_thread_bead_dangling_p(&mut pdf, &result)?;` line (Step 6) and
before `prune_after_subset(...)` (Step 7), insert:

```rust
    // Step 6.5: drop the dangling /P on annotations kept alive only through a
    // struct-tree OBJR /Obj (same structural-reference drop family). The OBJR
    // /Obj targets were collected by Step 5's struct-tree walk. Must run before
    // the prune so the now-unreferenced removed page is swept.
    drop_objr_obj_annot_dangling_p(&mut pdf, &result, &objr_obj_targets)?;
```

If Task 2 used the `_objr_obj_targets` placeholder name, rename it to
`objr_obj_targets` at the Step 5 call site now.

**Step 3: Build.**

Run: `cargo build -p flpdf-cli`
Expected: builds clean, no unused-variable warning.

**Step 4: Manual smoke check against the live fixture.**

```bash
target/debug/flpdf /tmp/u2kh/src.pdf --pages /tmp/u2kh/src.pdf 1,3 -- /tmp/u2kh/f2.pdf
qpdf --qdf --object-streams=disable --no-original-object-ids /tmp/u2kh/f2.pdf /tmp/u2kh/f2.qdf
grep -c "/Type /Page" /tmp/u2kh/f2.qdf   # expect 2 (was 3 before the fix)
grep "/P " /tmp/u2kh/f2.qdf               # annot must have no /P line
```
Expected: exactly 2 `/Type /Page`, annotation has no `/P`. (`/tmp/u2kh/src.pdf`
is the fixture built earlier; regenerate with `python3 /tmp/u2kh/gen.py` if absent.)

**Step 5: Commit.**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "feat(flpdf-cli): run OBJR /Obj annotation /P drop before subset prune (flpdf-u2kh)"
```

---

### Task 4: CLI oracle parity test vs qpdf 11.9.0

**Files:**
- Create: `crates/flpdf-cli/tests/cli_pages_objr_obj_annot_p_drop_qpdf.rs`

**Step 1: Write the oracle test** (model on `cli_pages_structtree_pg_drop_qpdf.rs`: same `qpdf_available`, `EXPECTED_QPDF_VERSION`, `run_qpdf`, `normalize_qdf`, `qdf_page_count`, `qdf_objects` helpers — copy them verbatim).

Fixture (mirror the empirically-verified topology, with a removed-page **and** a surviving-page OBJR annotation to prove no over-drop):

- `1` catalog `<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 10 0 R >>`
- `2` pages `<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >>`
- `3` page `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [31 0 R] >>`
- `4` page (removed) `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [30 0 R] >>`
- `5` page `<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>`
- `10` `<< /Type /StructTreeRoot /K 20 0 R >>`
- `20` `<< /Type /StructElem /S /Document /K [21 0 R 22 0 R] >>`
- `21` `<< /Type /OBJR /Pg 4 0 R /Obj 30 0 R >>`  (removed-page OBJR)
- `22` `<< /Type /OBJR /Pg 3 0 R /Obj 31 0 R >>`  (surviving-page OBJR)
- `30` `<< /Type /Annot /Subtype /Text /NM (rm) /P 4 0 R /Rect [0 0 10 10] >>`
- `31` `<< /Type /Annot /Subtype /Text /NM (kp) /P 3 0 R /Rect [0 0 10 10] >>`

Run both `qpdf src --pages . 1,3 -- out` and `flpdf src --pages . 1,3 -- out`,
normalize both, and assert with a shared `assert_facts(qdf, tool)`:

1. `qdf_page_count == 2` and exactly 2 `/Type /Page` objects (removed page GC'd,
   not nulled).
2. The annotation containing `(rm)` survives and has **no** `/P` line.
3. The annotation containing `(kp)` survives and **keeps** a `/P` line resolving
   to a `/Type /Page` object.
4. The OBJR reaching the `(rm)` annot keeps its `/Obj` (the annot survives).

Locate annots by `body.contains("(rm)")` / `body.contains("(kp)")` among
`/Type /Annot` objects (object numbers differ between tools).

**Step 2: Run the test.**

Run: `cargo test -p flpdf-cli --test cli_pages_objr_obj_annot_p_drop_qpdf`
Expected: PASS (qpdf 11.9.0 present in this environment, so it does not skip).
If it skips (wrong qpdf), the assertions never run — confirm `qpdf --version` is 11.9.0.

**Step 3: Commit.**

```bash
git add crates/flpdf-cli/tests/cli_pages_objr_obj_annot_p_drop_qpdf.rs
git commit -m "test(flpdf-cli): qpdf parity for OBJR /Obj annotation /P drop (flpdf-u2kh)"
```

---

### Task 5: Verification gate (fmt, clippy, full test, patch coverage, docs)

**Step 1: Format.**

Run: `cargo fmt --all` then `cargo fmt --all --check`
Expected: no diff. (CI quality gate is `cargo fmt --check` — see memory.)

**Step 2: Clippy.**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

**Step 3: Full test suite.**

Run: `cargo test -p flpdf -p flpdf-cli`
Expected: all pass, no regression in `struct_tree_pg` / `thread_bead_p` /
`page_extract*` / `cli_pages_*`.

**Step 4: Doc test + rustdoc links.**

Run: `cargo test -p flpdf --doc` and `cargo doc -p flpdf --no-deps 2>&1 | grep -i warning`
Expected: doc tests pass; no `broken_intra_doc_links` warning for the new module.
Confirm the public doc has no beads issue IDs / internal jargon (doc review rules):
Run: `grep -nE '(///|//!).*(flpdf-[0-9a-z.]+|epic|follow-up|TODO)' crates/flpdf/src/objr_obj_annot_p.rs` → expect no output.

**Step 5: Patch coverage gate.**

Commit all work first (gate diffs HEAD; dirty tree errors by design), then:
Run: `scripts/patch-coverage.sh --base main`
Expected: `flpdf` changed lines 100% covered. If any uncovered line is truly
untestable, mark `// cov:ignore: <reason>` and note it in the PR body; otherwise
add a unit test.

**Step 6: Final commit (if fmt/clippy touched anything).**

```bash
git add -A
git commit -m "chore(flpdf): fmt/clippy/coverage for OBJR /Obj annotation /P drop (flpdf-u2kh)"
```

---

## Acceptance criteria (from the beads issue design)

- Oracle test asserts flpdf output reaches observable parity with qpdf 11.9.0:
  removed page fully GC'd (no `null`), annotation `/P` dropped, OBJR `/Obj` kept.
- No regression in existing `struct_tree_pg` / `thread_bead_p` / page-ops tests.
- `cargo fmt --check` and `clippy -D warnings` pass; `flpdf` changed-line
  coverage is 100%.
- Public doc on `objr_obj_annot_p` is English-only, no internal tracker traces,
  with `# Errors` and a `# Scope` section.
```
