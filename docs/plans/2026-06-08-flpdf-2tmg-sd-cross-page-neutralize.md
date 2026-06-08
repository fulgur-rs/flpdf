# flpdf-2tmg: Neutralize /SD and cross-page /P vectors Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extend `extract_page`'s cross-page neutralization so a single-page extract no
longer leaks a sibling `/Page` stub via a GoTo `/SD` structure destination, a malformed
annotation `/P`, or an article-thread bead `/P` chain.

**Architecture:** All work is in `crates/flpdf/src/page_extract.rs`. The leak mechanism is
already established: `page_object_closure` copies any reachable sibling Page as a stub, then
`neutralize_absent_dests` drops dead references so `sweep_unreachable_objects` can prune the
stub. We add three references to the "drop when it targets an absent page" set: GoTo `/SD`
(drop the key, issue-prescribed), annotation `/P` (drop the key), and bead `/P` reached by
walking the page `/B` ring. The unifying predicate is "this reference resolves to a Page
other than `keep`".

**Tech Stack:** Rust, flpdf internal `Pdf`/`Object`/`Dictionary` API,
`outline_dest_remap::resolve_ref_chain` (pub(crate), follows indirection up to depth 64 and
returns the terminal `ObjectRef`).

**Reference rules:** Follow `.claude/rules/pdf-rust-review-patterns.md` — resolve indirect
references before matching (#2), bound graph traversal with a visited set and only walk from
known positions, never brute-force all live objects (#4), avoid needless `.clone()` (#1).

---

## Background facts (read before starting)

- `neutralize_action_chain` (page_extract.rs ~326): GoTo handling lives in the `is_goto`
  block (~374), where `/D` is taken by `remove`, tested with `dest_targets_absent_page`, and
  re-inserted if it stays. `/SD` is added here, in parallel, independently.
- `dest_page_ref_resolved` (outline_dest_remap.rs:1002) returns the **first array element as
  the page ref**. It MUST NOT be used for `/SD`: an `/SD` array's first element is a
  *StructElem* ref, not a page. A separate resolver does one extra hop (StructElem `/Pg`).
- `resolve_ref_chain(target, &obj)` returns `(concrete_object, Option<terminal_ref>)`. Use it
  for every indirection chain; the terminal ref is the resolved page ref for a `/Pg` or `/P`.
- `neutralize_absent_dests` (page_extract.rs:169) already detaches the page's `/Annots` and
  `/AA`. Add `/B` (bead ring) handling there. `neutralize_annot_if_absent` (page_extract.rs:214)
  is where annotation `/P` is added.
- Test infra in `crates/flpdf/tests/page_extract_tests.rs`: `build_pdf(&[(obj_num, body)], root)`
  builds a PDF; `count_type(&mut doc, b"Page")` counts objects of a `/Type`. Templates:
  `cross_page_link_neutralized_no_sibling_leak`, `self_page_link_is_preserved`,
  `action_goto_absent_page_is_neutralized`.
- Run a single test with:
  `cargo test -p flpdf --test page_extract_tests <name> -- --exact --nocapture`

---

## Task 1: GoTo `/SD` structure destination neutralization

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs` (add resolver; extend `is_goto` block ~374)
- Test: `crates/flpdf/tests/page_extract_tests.rs` (append two tests)

**Step 1: Write the failing tests**

Append to `crates/flpdf/tests/page_extract_tests.rs`. Page 0 (obj 3) holds a Link whose
GoTo `/SD` points at a StructElem (obj 8) whose `/Pg` is the sibling page (obj 4). Nothing
else references the sibling, so it must be pruned.

```rust
/// GoTo /SD -> StructElem(/Pg sibling) keeps the sibling reachable unless /SD is
/// neutralized. (flpdf-2tmg, ISO 32000-2 §12.6.4.3.)
fn cross_page_sd_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /SD [8 0 R /Fit] >> >>"),
            (8, "<< /Type /StructElem /S /P /Pg 4 0 R >>"),
        ],
        1,
    )
}

#[test]
fn action_goto_sd_absent_page_is_neutralized() {
    let mut src = Pdf::open(std::io::Cursor::new(cross_page_sd_pdf())).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "GoTo /SD sibling must be pruned"
    );
    // The StructElem (reachable only via /SD) is swept too.
    assert_eq!(
        count_type(&mut out, b"StructElem"),
        0,
        "StructElem reachable only via the neutralized /SD must be swept"
    );
    // Action is retained, /SD removed.
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").unwrap().as_dict().unwrap();
    assert_eq!(
        action.get("S"),
        Some(&flpdf::Object::Name(b"GoTo".to_vec())),
        "GoTo action retained"
    );
    assert!(action.get("SD").is_none(), "/SD must be neutralized (removed)");
}

#[test]
fn action_goto_sd_self_page_is_preserved() {
    // /SD -> StructElem whose /Pg is the extracted page itself -> kept.
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /SD [8 0 R /Fit] >> >>"),
            (8, "<< /Type /StructElem /S /P /Pg 3 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    let action = annot.get("A").unwrap().as_dict().unwrap();
    assert!(
        action.get("SD").is_some(),
        "self-page /SD must be preserved"
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p flpdf --test page_extract_tests action_goto_sd -- --nocapture`
Expected: `action_goto_sd_absent_page_is_neutralized` FAILS (Page count == 2, /SD present).
`action_goto_sd_self_page_is_preserved` should PASS already (self-page never leaked), but
confirm — if it fails on compile, fix the test, not the source.

**Step 3: Write the resolver + wire it in**

Add the resolver near `dest_targets_absent_page` (page_extract.rs ~447):

```rust
/// `true` when a GoTo `/SD` structure destination resolves to a page other than
/// `keep`. An `/SD` value is `[structElemRef /Fit ...]` (or an indirect ref to
/// one); the first element is a *structure element*, whose `/Pg` is the target
/// page (ISO 32000-2 §12.6.4.3). Named structure destinations (a name/string,
/// resolved via the structure tree) carry no in-document page ref and return
/// `false`. A missing / unresolvable / non-Page `/Pg`, or a `/Pg` pointing at
/// `keep`, returns `false` (kept conservatively). Each level may be indirect;
/// `resolve_ref_chain` bounds the indirection.
fn sd_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    sd: &Object,
    keep: ObjectRef,
) -> Result<bool> {
    let (concrete, _) = resolve_ref_chain(target, sd)?;
    let Object::Array(arr) = concrete else {
        return Ok(false); // named structure destination or malformed
    };
    let Some(struct_elem) = arr.into_iter().next() else {
        return Ok(false);
    };
    let (se, _) = resolve_ref_chain(target, &struct_elem)?;
    let Some(se_dict) = se.into_dict() else {
        return Ok(false);
    };
    let Some(pg) = se_dict.get("Pg").cloned() else {
        return Ok(false);
    };
    let (pg_concrete, pg_ref) = resolve_ref_chain(target, &pg)?;
    Ok(match pg_ref {
        Some(r) => r != keep && is_page_dict(&pg_concrete),
        None => false,
    })
}

/// `true` when `obj` is a `<< /Type /Page ... >>` dictionary.
fn is_page_dict(obj: &Object) -> bool {
    obj.as_dict()
        .and_then(|d| d.get("Type"))
        .is_some_and(|t| matches!(t, Object::Name(n) if n == b"Page"))
}
```

In `neutralize_action_chain`, inside the existing `if is_goto { ... }` block, AFTER the `/D`
handling, add the `/SD` arm (independent — a GoTo may carry both; drop only the dead one):

```rust
        if let Some(sd_val) = act.remove("SD") {
            if sd_targets_absent_page(target, &sd_val, keep)? {
                changed = true;
            } else {
                act.insert("SD", sd_val);
            }
        }
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p flpdf --test page_extract_tests action_goto_sd -- --nocapture`
Expected: both PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_extract.rs crates/flpdf/tests/page_extract_tests.rs
git commit -m "fix: neutralize GoTo /SD structure dests targeting absent pages (flpdf-2tmg)"
```

---

## Task 2: Annotation `/P` neutralization

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs` (add `/P` predicate; extend `neutralize_annot_if_absent`)
- Test: `crates/flpdf/tests/page_extract_tests.rs` (append two tests)

**Step 1: Write the failing tests**

```rust
#[test]
fn annot_p_absent_page_is_neutralized() {
    // A malformed annotation /P points at the SIBLING page (obj 4); the closure
    // copies the sibling as a stub. Dropping /P makes it unreachable.
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (5, "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /P 4 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "sibling reached via annotation /P must be pruned"
    );
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    assert!(annot.get("P").is_none(), "absent-page /P must be dropped");
}

#[test]
fn annot_p_self_page_is_preserved() {
    // /P points at the extracted page itself: kept (remapped to the new ref).
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [5 0 R] >>"),
            (5, "<< /Type /Annot /Subtype /Text /Rect [0 0 10 10] /P 3 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(count_type(&mut out, b"Page"), 1);
    let leaf = only_leaf(&mut out);
    let annot_ref = match leaf.get("Annots") {
        Some(flpdf::Object::Array(a)) => a[0].as_ref_id().unwrap(),
        other => panic!("expected /Annots array, got {other:?}"),
    };
    let annot = out.resolve(annot_ref).unwrap().into_dict().unwrap();
    assert!(annot.get("P").is_some(), "self-page /P must be preserved");
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --test page_extract_tests annot_p_ -- --nocapture`
Expected: `annot_p_absent_page_is_neutralized` FAILS (Page count == 2).

**Step 3: Implement**

Add the shared `/P` predicate near `sd_targets_absent_page`:

```rust
/// `true` when `p` (an annotation's or bead's `/P`) resolves to a Page object
/// other than `keep`. `/P` always denotes "the page this object belongs to"
/// (ISO 32000-2); a `/P` pointing at an absent page is dangling and dropped.
/// Non-Page / unresolvable / `keep` targets return `false` (kept).
fn p_targets_absent_page(
    target: &mut Pdf<Cursor<Vec<u8>>>,
    p: &Object,
    keep: ObjectRef,
) -> Result<bool> {
    let (concrete, p_ref) = resolve_ref_chain(target, p)?;
    Ok(match p_ref {
        Some(r) => r != keep && is_page_dict(&concrete),
        None => false,
    })
}
```

In `neutralize_annot_if_absent`, after the `/AA` handling and before the `if changed`
block, add (the annot dict is already owned; take by `remove`, re-insert if it stays):

```rust
    // /P — the page this annotation belongs to. A malformed /P pointing at an
    // absent (sibling) page keeps that page's stub reachable; drop it.
    if let Some(p_val) = annot.remove("P") {
        if p_targets_absent_page(target, &p_val, keep)? {
            changed = true;
        } else {
            annot.insert("P", p_val);
        }
    }
```

**Step 4: Run to verify pass**

Run: `cargo test -p flpdf --test page_extract_tests annot_p_ -- --nocapture`
Expected: both PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_extract.rs crates/flpdf/tests/page_extract_tests.rs
git commit -m "fix: drop annotation /P targeting absent pages on extract (flpdf-2tmg)"
```

---

## Task 3: Article-thread bead `/P` ring walk

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs` (add ring-walk fn; call it from `neutralize_absent_dests`)
- Test: `crates/flpdf/tests/page_extract_tests.rs` (append one test)

**Step 1: Write the failing test**

Page 0 (obj 3) `/B` lists its on-page bead (obj 10). The thread ring `/N`s to a sibling bead
(obj 11) whose `/P` is the sibling page (obj 4). Walking the ring and dropping bead 11's `/P`
makes the sibling unreachable. The `/B` array and bead ring are RETAINED (qpdf parity).

```rust
#[test]
fn bead_p_absent_page_is_neutralized() {
    let pdf = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [10 0 R] >>"),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /B [11 0 R] >>"),
            // Bead ring: 10 (on kept page) <-> 11 (on sibling page).
            (10, "<< /T 12 0 R /N 11 0 R /V 11 0 R /P 3 0 R /R [0 0 10 10] >>"),
            (11, "<< /T 12 0 R /N 10 0 R /V 10 0 R /P 4 0 R /R [0 0 10 10] >>"),
            (12, "<< /T (Article) /F 10 0 R >>"),
        ],
        1,
    );
    let mut src = Pdf::open(std::io::Cursor::new(pdf)).unwrap();
    let mut out = extract_page(&mut src, 0).unwrap();
    assert_eq!(
        count_type(&mut out, b"Page"),
        1,
        "sibling reached via bead /P must be pruned"
    );
    // The kept page's /B is retained (qpdf keeps the ring).
    let leaf = only_leaf(&mut out);
    assert!(leaf.get("B").is_some(), "page /B must be retained");
}
```

**Step 2: Run to verify failure**

Run: `cargo test -p flpdf --test page_extract_tests bead_p_ -- --nocapture`
Expected: FAILS (Page count == 2 — sibling bead 11's `/P` still reaches obj 4).

**Step 3: Implement the ring walk**

Add the function (uses the `p_targets_absent_page` predicate from Task 2):

```rust
/// Walk the article-thread bead ring reachable from this page's `/B` and drop
/// each bead's `/P` that targets an absent page. `/N`/`/V` link beads (not
/// pages), so they never leak; only the page-valued `/P` is neutralized. The
/// ring is bounded by `visited` (each bead handled once). The `/B` array and
/// the beads themselves are retained — only dangling `/P` keys are dropped,
/// matching qpdf's single-page output.
fn neutralize_bead_ring(target: &mut Pdf<Cursor<Vec<u8>>>, page_ref: ObjectRef) -> Result<()> {
    let b_val = {
        let page_obj = target.resolve_borrowed(page_ref)?;
        let Some(page_dict) = page_obj.as_dict() else {
            return Ok(());
        };
        page_dict.get("B").cloned()
    };
    let mut queue: Vec<ObjectRef> = match b_val {
        Some(Object::Array(arr)) => arr.iter().filter_map(Object::as_ref_id).collect(),
        Some(Object::Reference(r)) => match target.resolve_borrowed(r)? {
            Object::Array(arr) => arr.iter().filter_map(Object::as_ref_id).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    while let Some(bead_ref) = queue.pop() {
        if !visited.insert(bead_ref) {
            continue;
        }
        let Some(mut bead) = target.resolve_borrowed(bead_ref)?.as_dict().cloned() else {
            continue;
        };
        // Enqueue ring neighbours before mutating.
        for key in ["N", "V"] {
            if let Some(Object::Reference(r)) = bead.get(key) {
                queue.push(*r);
            }
        }
        if let Some(p_val) = bead.remove("P") {
            if p_targets_absent_page(target, &p_val, page_ref)? {
                target.set_object(bead_ref, Object::Dictionary(bead));
            } else {
                bead.insert("P", p_val);
            }
        }
    }
    Ok(())
}
```

Call it from `neutralize_absent_dests`, just before the final `Ok(())` (after the page `/AA`
handling):

```rust
    neutralize_bead_ring(target, page_ref)?;
    Ok(())
```

**Step 4: Run to verify pass**

Run: `cargo test -p flpdf --test page_extract_tests bead_p_ -- --nocapture`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/page_extract.rs crates/flpdf/tests/page_extract_tests.rs
git commit -m "fix: drop article-thread bead /P targeting absent pages on extract (flpdf-2tmg)"
```

---

## Task 4: Update the module doc

**Files:**
- Modify: `crates/flpdf/src/page_extract.rs:27-30` (module doc)

**Step 1: Edit the doc**

The current text (lines 27-30) claims `/SD` is not inspected — now false. Replace:

```
//! Only explicit page destinations (`/D`) are neutralized. A GoTo action's
//! structure destination (`/SD`, ISO 32000-2 §12.6.4.3) is not inspected, so a
//! `/SD` pointing into another page's structure tree can keep that page
//! reachable in the output.
```

with:

```
//! Both explicit page destinations (`/D`) and a GoTo action's structure
//! destination (`/SD`, ISO 32000-2 §12.6.4.3, resolved through its StructElem's
//! `/Pg`) are neutralized. A malformed annotation `/P` and an article-thread
//! bead `/P` (reached through the page's `/B` ring) that point at an absent page
//! are likewise dropped. Article-thread `/B` arrays and bead rings are otherwise
//! retained, matching qpdf's single-page output.
```

**Step 2: Verify it builds and doctests are clean**

Run: `cargo doc -p flpdf --no-deps 2>&1 | grep -i warning` — expect no new warnings.

**Step 3: Commit**

```bash
git add crates/flpdf/src/page_extract.rs
git commit -m "docs: update extract module doc for /SD and /P neutralization (flpdf-2tmg)"
```

---

## Task 5: Full verification

**Step 1: Run the whole flpdf test suite**

Run: `cargo test -p flpdf 2>&1 | grep -E "test result|error\[|FAILED"`
Expected: all pass, no failures.

**Step 2: Clippy + fmt**

Run: `cargo clippy -p flpdf --all-targets 2>&1 | grep -E "warning|error" | head`
Expected: no new warnings.
Run: `cargo fmt -p flpdf -- --check`
Expected: clean (run `cargo fmt -p flpdf` if not).

**Step 3: Confirm the three probes drop Page count 2 -> 1**

Run: `cargo test -p flpdf --test page_extract_tests -- _sd_ annot_p_ bead_p_ --nocapture`
Expected: all neutralization + retention tests pass.

**Step 4: Commit any fmt fixes**

```bash
git add -A
git commit -m "style: cargo fmt (flpdf-2tmg)" || true
```

---

## Out of scope (do NOT implement)

- `outline_dest_remap` `/SD` handling — issue says "consistent with ... deferred".
- Catalog `/Threads` — already absent from the minimal target catalog; nothing to do.
- Splicing bead rings or rewriting `/P` to `keep` — qpdf drops the dangling key, it does not
  splice or rewrite; we match that.
