# Re-linearize Reachability GC Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `flpdf rewrite --linearize` byte-identical to `qpdf --linearize` when the
input is ALREADY linearized, by garbage-collecting the source's old `/Linearized` parameter
dict and old hint stream (currently leaked into the second half) — matching qpdf's
reachability GC (beads flpdf-phfu).

**Architecture:** `LinearizationPlan::from_pdf` builds its object universe `all_refs` from
`pdf.object_refs()` (the full source xref) with no reachability GC, so unreachable source
linearization artifacts survive. Add a trailer-seeded reachability helper in
`rewrite_renumber.rs` (reusing the existing `collect_refs` walk, seeds INCLUDING `/Encrypt`)
and intersect `all_refs` with the reachable set. For non-re-linearized inputs every object is
reachable, so the universe — and every existing golden — is unchanged.

**Tech Stack:** Rust, qpdf 11.9.0 oracle, `qpdf-zlib-compat` feature for byte-identity,
beads, cargo llvm-cov patch-coverage.

**Verified root cause (qpdf raw-oracle, see beads design field):** re-linearizing
`linearized-one-page.pdf`, qpdf emits 9 objects (/Size 10), flpdf 11 (/Size 12). flpdf's 2
extra objects are the source's old `/Linearized` param dict (source obj 3) and old primary
hint stream (source obj 5), both unreachable from Root/Info, landing in `part4_rest`.

---

### Task 1: Reachability helper in `rewrite_renumber.rs`

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs` (add `reachable_object_set`, ~after `CatalogFirstRenumber::build_excluding` at line 143)
- Test: same file, `#[cfg(test)] mod tests` (existing module at line 499)

**Step 1: Write the failing unit test**

Add to the test module:

```rust
#[test]
fn reachable_object_set_drops_source_linearization_artifacts() {
    // linearized-one-page.pdf is a qpdf-produced linearized one-page PDF whose
    // source objects are: 1=Pages, 2=Info, 3=/Linearized param dict, 4=Catalog,
    // 5=primary hint stream, 6=Page, 7..9=content/resources/font. The param dict
    // (obj 3) and the hint stream (obj 5) are unreachable from Root (4) / Info (2):
    // /H is a byte offset, not an object ref. qpdf garbage-collects them when
    // re-linearizing; reachable_object_set must too (flpdf-phfu).
    let bytes = include_bytes!("../../../tests/fixtures/compat/linearized-one-page.pdf");
    let mut pdf = Pdf::open_mem(bytes).expect("open");
    let reachable = reachable_object_set(&mut pdf, &BTreeSet::new()).expect("walk");
    let mut nums: Vec<u32> = reachable.iter().map(|r| r.number).collect();
    nums.sort_unstable();
    assert_eq!(nums, vec![1, 2, 4, 6, 7, 8, 9], "old lin dict (3) + hint stream (5) must be GC'd");
    assert!(!reachable.contains(&ObjectRef::new(3, 0)));
    assert!(!reachable.contains(&ObjectRef::new(5, 0)));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib reachable_object_set_drops_source_linearization_artifacts`
Expected: FAIL — `reachable_object_set` not defined.

**Step 3: Write minimal implementation**

Insert after the `impl CatalogFirstRenumber { ... }` block (after line 143, before the
`#[cfg(test)] impl CatalogFirstRenumber` test-only block) — a free function in the module:

```rust
/// Compute the set of object references reachable from the trailer roots,
/// matching qpdf's reachability garbage collection of the linearized object
/// universe.
///
/// Seeds from `/Root` plus every indirect trailer entry — **including
/// `/Encrypt`**, excluding `/Prev`, `/Size`, `/ID` (and `/Root`, already
/// seeded) — then BFS via [`collect_refs`] (which follows every inline
/// reference, `/Length` included). References in `excluded` (e.g. orphaned
/// indirect `/Length` holders) are skipped — neither recorded nor walked — so
/// an object reachable ONLY through a dead `/Length` edge is correctly absent.
///
/// Unlike [`CatalogFirstRenumber`], `/Encrypt` IS seeded: the linearized object
/// universe must retain the encryption dictionary and its closure (the plain
/// rewrite numbers `/Encrypt` in a separate slot, hence its omission there).
///
/// # Errors
///
/// Propagates [`Error::Io`] / [`Error::Parse`] / [`Error::Encrypted`] from
/// resolving objects during the walk, and [`Error::Unsupported`] if inline
/// nesting exceeds [`MAX_INLINE_DEPTH`] (via [`collect_refs`]) or the trailer
/// has no `/Root`.
pub(crate) fn reachable_object_set<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    excluded: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let root = pdf
        .root_ref()
        .ok_or_else(|| Error::Unsupported("reachability: trailer has no /Root".to_string()))?;
    let mut seeds: Vec<ObjectRef> = vec![root];
    for (key, value) in pdf.trailer().iter() {
        // /Encrypt is intentionally NOT skipped (it is part of the live universe);
        // /Prev, /Size, /ID, /Root are not object roots of the document graph.
        if matches!(key, b"ID" | b"Prev" | b"Root" | b"Size") {
            continue;
        }
        if let Object::Reference(r) = value {
            seeds.push(*r);
        }
    }

    let mut reachable: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();
    for seed in seeds {
        if !excluded.contains(&seed) && reachable.insert(seed) {
            queue.push_back(seed);
        }
    }
    while let Some(cur) = queue.pop_front() {
        let obj = pdf.resolve_borrowed(cur)?;
        collect_refs(obj, 0, &mut |r| {
            if !excluded.contains(&r) && reachable.insert(r) {
                queue.push_back(r);
            }
        })?;
    }
    Ok(reachable)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib reachable_object_set`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/rewrite_renumber.rs
git commit -m "feat(linearize): add trailer-seeded reachable_object_set GC helper (flpdf-phfu)"
```

---

### Task 2: Wire the reachability filter into `LinearizationPlan::from_pdf`

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:558-572` (the `all_refs` build loop)

**Step 1: Write the failing integration test (object-count parity)**

The byte test (Task 4) is the real proof but needs the golden + feature. Add a cheap,
feature-free structural test now (catches the object-count regression on the default build).
Put it in `crates/flpdf/tests/linearize_classic_tests.rs` (reuses its `Pdf` + plan harness):

```rust
/// Re-linearizing an already-linearized input must not over-populate the second
/// half: qpdf GCs the source's old /Linearized dict + hint stream, so the plan's
/// object universe is the 7 reachable objects, NOT 9 (flpdf-phfu).
#[test]
fn relinearize_drops_source_linearization_artifacts_from_universe() {
    let path = fixture_path("linearized-one-page.pdf");
    let mut pdf = flpdf::Pdf::open(std::io::BufReader::new(
        std::fs::File::open(&path).unwrap(),
    ))
    .unwrap();
    let plan = flpdf::linearization::LinearizationPlan::from_pdf(&mut pdf, false).unwrap();
    // total_object_count = reachable universe size; 7 (1,2,4,6,7,8,9), not 9.
    assert_eq!(plan.total_object_count(), 7);
}
```

(If `linearize_classic_tests.rs` has no `fixture_path` / `total_object_count` accessor, mirror
whatever helper it already uses, and if `total_object_count` is private, assert via the public
surface the file already exercises — see Step 1a.)

**Step 1a: Confirm the assertion handle**

Run: `grep -nE 'fixture_path|total_object_count|pub .*total' crates/flpdf/tests/linearize_classic_tests.rs crates/flpdf/src/linearization/plan.rs`
If `total_object_count` is not public, either (a) add a `pub fn total_object_count(&self) -> u32`
accessor returning the field, or (b) assert object-count via the byte test only and make this a
plan-internal `#[cfg(test)]` unit test inside plan.rs instead. Prefer (a) only if a public
accessor is warranted; otherwise put this assertion as a unit test in `plan.rs`'s test module
where the private field is in scope.

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf relinearize_drops_source_linearization_artifacts_from_universe`
Expected: FAIL — `assert_eq!(.., 7)` sees 9 (the 2 artifacts still present).

**Step 3: Implement the filter**

In `plan.rs:from_pdf`, right after `let orphan_length_holders = orphaned_indirect_length_holders(pdf)?;`
(line 558), compute the reachable set; then add the filter in the build loop. Final loop:

```rust
let orphan_length_holders = orphaned_indirect_length_holders(pdf)?;
// qpdf garbage-collects objects unreachable from the trailer roots (it only
// enqueues reachable objects). The plain full-rewrite path does this via
// CatalogFirstRenumber's trailer-seeded BFS; the linearize universe must too,
// or a re-linearized source leaks its old /Linearized param dict + hint stream
// (unreachable artifacts) into the second half (flpdf-phfu).
let reachable = crate::rewrite_renumber::reachable_object_set(pdf, &orphan_length_holders)?;
let object_refs = pdf.object_refs();
let mut all_refs: Vec<ObjectRef> = Vec::with_capacity(object_refs.len());
for r in object_refs {
    if r.number == 0 {
        continue;
    }
    if orphan_length_holders.contains(&r) {
        continue;
    }
    if crate::writer::is_source_structural_container(pdf.resolve_borrowed(r)?) {
        continue;
    }
    if !reachable.contains(&r) {
        // Unreachable from the trailer roots — qpdf drops it (flpdf-phfu).
        continue;
    }
    all_refs.push(r);
}
```

(Order: the `!reachable` check stays AFTER `is_source_structural_container` so the existing
structural-container drop branch keeps its coverage — ObjStm/XRef containers are also
unreachable, but they must continue to be dropped by the structural check, not the new one.)

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf relinearize_drops_source_linearization_artifacts_from_universe`
Expected: PASS.

Also run the existing classic + plan suites to confirm no regression on default build:
Run: `cargo test -p flpdf --lib linearization::plan && cargo test -p flpdf --test linearize_classic_tests`
Expected: all PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs crates/flpdf/tests/linearize_classic_tests.rs
git commit -m "fix(linearize): GC unreachable source lin-artifacts from re-linearize universe (flpdf-phfu)"
```

---

### Task 3: Add the qpdf golden + regenerate.sh entry

**Files:**
- Create: `tests/golden/references/linearized-one-page/linearize.pdf` (qpdf output)
- Modify: `tests/golden/regenerate.sh` (add the generation command near the other
  `<stem>/linearize.pdf` blocks, ~line 425+)

**Step 1: Generate the golden with the pinned qpdf**

```bash
qpdf --version   # MUST be 11.9.0
qpdf --linearize --deterministic-id --warning-exit-0 \
    tests/fixtures/compat/linearized-one-page.pdf \
    tests/golden/references/linearized-one-page/linearize.pdf
```

Expected: 1701-byte, 9-object output (param dict obj 3, hint stream obj 5).

**Step 2: Add the regenerate.sh entry** (mirror the existing `one-page/linearize.pdf` block):

```bash
qpdf --linearize --deterministic-id --warning-exit-0 \
    "$FIX/linearized-one-page.pdf" "$REF/linearized-one-page/linearize.pdf"
echo "linearized-one-page/linearize.pdf"
```

**Step 3: Sanity-check the golden object count**

Run: `grep -acE '[0-9]+ [0-9]+ obj' tests/golden/references/linearized-one-page/linearize.pdf`
Expected: `9`.

**Step 4: Commit**

```bash
git add tests/golden/references/linearized-one-page/linearize.pdf tests/golden/regenerate.sh
git commit -m "test(golden): add re-linearize golden for linearized-one-page (flpdf-phfu)"
```

---

### Task 4: Byte-identity test (qpdf-zlib-compat) + CI list

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_tests.rs` (add a test using the existing
  `flpdf_linearized` / `golden` helpers)
- Modify: `.github/workflows/ci.yml` (add the test to the explicit bytes-identical list —
  gated byte tests are not auto-discovered; see memory flpdf-ci-bytes-identical-explicit-test-list)

**Step 1: Write the byte test**

```rust
/// Re-linearizing an already-linearized input is byte-identical to
/// `qpdf --linearize --deterministic-id` — the source's old /Linearized param
/// dict and hint stream are GC'd, not leaked into the second half (flpdf-phfu).
#[test]
fn relinearize_one_page_byte_identical() {
    let out = flpdf_linearized("linearized-one-page.pdf");
    let want = golden("linearized-one-page");
    assert_bytes_eq(&out, &want); // or assert_eq!(out, want) per the file's convention
}
```

(Check the file's existing assertion helper name first; reuse it for the first-diff report
that surfaces the `/ID[0]`-digest masking trap.)

**Step 2: Run with the feature**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests relinearize_one_page_byte_identical`
Expected: PASS (full byte cmp). If it fails at `/ID[0]` with equal size, that is the
downstream-layout-divergence signal (memory flpdf-linearize-id0-digest-masks-downstream-divergence)
— investigate the body, do NOT treat it as an /ID bug.

**Step 3: Add to CI explicit list**

Run: `grep -nE 'cmp_linearize|qpdf-zlib-compat' .github/workflows/ci.yml`
Add `relinearize_one_page_byte_identical` (or confirm the whole `cmp_linearize_tests` target
is already run under the feature — if the target is listed wholesale, no edit needed).

**Step 4: Commit**

```bash
git add crates/flpdf/tests/cmp_linearize_tests.rs .github/workflows/ci.yml
git commit -m "test(linearize): pin re-linearize byte-identity to qpdf golden (flpdf-phfu)"
```

---

### Task 5: Full regression + patch coverage gate

**Step 1: Full linearize byte suite under the feature (no regression)**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests --test cmp_linearize_objstm_tests --test linearize_classic_tests`
Expected: all PASS (existing goldens unchanged — every object was already reachable for
non-re-linearized inputs).

**Step 2: Default-build full crate test**

Run: `cargo test -p flpdf`
Expected: all PASS.

**Step 3: fmt + clippy**

Run: `cargo fmt --all && cargo clippy -p flpdf --all-targets -- -D warnings`
Expected: clean (memory flpdf-ci-quality-fmt-check: fmt must pass before push).

**Step 4: Patch coverage (WITHOUT the feature)**

Commit all work first, then:
Run: `scripts/patch-coverage.sh --base main`
Expected: changed lines in `flpdf` 100% covered. The new `reachable_object_set` is covered by
the Task 1 unit test; the plan.rs filter line by the Task 2 test. If the `Err`/`ok_or_else`
no-`/Root` arm is uncovered and truly untestable, `// cov:ignore: <reason>` and note it in the
PR. (llvm-cov runs without qpdf-zlib-compat — memory llvm-cov-no-qpdf-zlib-compat.)

**Step 5: Final commit (if fmt/clippy touched anything)**

```bash
git add -A && git commit -m "chore: fmt/clippy after flpdf-phfu fix" || true
```

---

## Done criteria

- `relinearize_one_page_byte_identical` passes under `qpdf-zlib-compat` (byte-identical to qpdf).
- All existing linearize byte goldens still pass (no regression).
- `reachable_object_set` unit test + plan-universe object-count test pass on the default build.
- patch-coverage: flpdf changed lines 100%.
- fmt + clippy clean.
