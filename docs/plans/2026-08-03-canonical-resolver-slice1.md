# Canonical resolver, slice 1: ownership and uncompressed objects (flpdf-25kg.3.5)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** stand up `reader/resolver.rs` with the ownership model and borrow
discipline, resolving **uncompressed (xref type 1) objects only**, so the
riskiest part of `flpdf-25kg.3.5` is proven end-to-end before the remaining
source classes are added.

**Architecture:** `Pdf` owns `Rc<ResolverHandle<R>>`; `ResolverHandle` wraps
`RefCell<ResolverCore<R>>` and implements `DocumentResolver`. `resolve_indirect`
mirrors `QPDF::resolve` in three phases so no borrow spans a nested resolution.

**Design of record:** `docs/plans/2026-08-03-canonical-resolver-ownership-design.md`
and `flpdf-25kg.3.5`'s design field. Read both before starting.

---

## Scope

**In:** `ResolverCore`/`ResolverHandle`, the `Weak` attach, the in-progress
guard, xref type 1 resolution through the three phases, and the re-entrancy
regression.

**Out, deliberately:** ObjStm (type 2), encrypted documents, xref streams,
free/missing edge cases, `Pdf::get_object` publication, and any legacy removal.
Later slices add those on the same skeleton.

**Why this is safe to land partial.** `try_dereference` and the `try_as_*`
family have **zero production callers** — confirmed while landing
`flpdf-25kg.3.4`, whose own native entry points are `pub(crate)` and callerless
until this issue wires them. Production resolution still runs through the
legacy `resolve_object_handle`/`resolve_borrowed` path, untouched. So a resolver
that only understands type 1 cannot regress production; it can only affect
tests, which is exactly what we want while the ownership model is being proven.

**Do not** make the canonical resolver fall back to the legacy path for the
classes it does not yet handle. That is the resolver bridge `flpdf-25kg.3.5`'s
acceptance criteria forbid. An unhandled class returns an error in this slice
and gains real support in a later one.

---

## Task 1: A constructor carrying both identity and resolver

**Files:** `crates/flpdf/src/object_handle.rs`

`new_indirect_with_resolver` (`:287`) sets `pdf_unique_id: None`;
`new_indirect_unresolved_for_pdf` (`:258`) sets the identity but no resolver.
`get_object_handle` needs both, and the identity cannot be dropped to gain a
resolver.

> **Corrected during implementation.** A draft of this task said
> `is_canonical_object_handle` (`reader.rs:1618`) compares on `pdf_unique_id`.
> It does not — it looks the ref up in `handle_registry` and compares `Rc`
> pointers through `is_same_object_as`. The conclusion survives through the
> other half: `belongs_to_pdf` (`:418`) and `containing_object_refs_for_pdf`
> (`:405`) back the foreign-object rejection in `mark_object_handle_dirty`
> (`:1785`, `:1790`), `filespec_helper.rs:114`, and `embedded_files.rs:492`,
> and `set_resolved` stamps the slot's identity onto every direct child, so a
> `None` identity poisons children too. Measured rather than argued: patching
> `new_indirect_unresolved_for_pdf` to discard its argument fails 61 tests.

**Step 1: failing test** — construct a handle with both, assert the identity is
preserved and the resolver reachable.

**Step 2:** run it, confirm it fails to compile for the right reason.

**Step 3:** add the combined constructor. Prefer extending the existing private
`new_indirect_unresolved_with_identity` rather than adding a fourth public-ish
entry point; the existing two should end up delegating to one body.

**Step 4:** `cargo test -p flpdf --lib object_handle`.

**Step 5:** commit.

---

## Task 2: `ResolverCore` and the attach

**Files:** create `crates/flpdf/src/reader/resolver.rs`; modify
`crates/flpdf/src/reader.rs`, `crates/flpdf/src/lib.rs`.

**Step 1: failing test** — open a real `Pdf`, take a handle from
`get_object_handle`, and assert `try_dereference` reaches the resolver rather
than reporting a dropped document. It must fail *because the resolver rejects
the class*, not because there is no resolver — those are different errors and
the test must distinguish them.

**Step 2:** run it; expect the dropped-PDF error, since handles carry
`resolver: None` today.

**Step 3:** implement.

`ResolverCore<R>` holds only what the design enumerates from qpdf: the input
source, the xref table, the canonical `ObjectRef → ObjectHandle` cache, the
in-progress set, the resolved-ObjStm set, the recovery policy, and the warning
sink. Nothing else moves in this slice — in particular the legacy `cache`,
`legacy_materialized_memo`, and `trailer_handle_memo` stay on `Pdf` and stay
marked for deletion.

`ResolverHandle<R>` wraps `RefCell<ResolverCore<R>>` and implements
`DocumentResolver`. `Pdf` holds `Rc<ResolverHandle<R>>` and `get_object_handle`
hands out `Rc::downgrade(...)` via Task 1's constructor.

For this slice `resolve_indirect` may return
`Error::Unsupported` for every class; Task 4 makes type 1 real.

**Step 4:** the test now fails on the class rejection, not on a missing
resolver. Also assert the drop path still works: dropping the `Pdf` must make
the handle read as null via `Destroyed`, not error — that is `flpdf-nrp3`'s
recorded divergence and this slice must not silently change it.

**Step 5:** commit.

---

## Task 3: The in-progress guard

**Files:** `crates/flpdf/src/reader/resolver.rs`

qpdf's `ResolveRecorder` (`QPDF.hh:980-996`) inserts into `m->resolving` in its
constructor and erases in its destructor; `QPDF::resolve`
(`QPDF.cc:1706-1712`) treats a hit as a loop, warns
`"loop detected resolving object N G"`, and caches null.

**Step 1: failing test** — re-entering `resolve_indirect` for a reference
already in progress yields the loop outcome, and the mark is gone afterwards.
Add a second test that the mark is removed when the inner resolution returns an
error, not only on success.

**Step 2:** run; expect no guard to exist.

**Step 3:** implement a `Drop`-guard type. The unwind case is the reason it is a
guard rather than a matched insert/remove pair — assert it, do not assume it.

**Step 4:** `cargo test -p flpdf --lib resolver`.

**Step 5:** commit.

---

## Task 4: Uncompressed objects through the three phases

**Files:** `crates/flpdf/src/reader/resolver.rs`

**Step 1: failing test** — a real `Pdf` over a small in-memory document;
`try_dereference` on an uncompressed object resolves it, and a second call is a
no-op that does not re-read.

**Step 2:** run; expect the class rejection from Task 2.

**Step 3:** implement, mirroring `QPDF::resolve`:

1. short borrow — `isUnresolved` test, loop guard, insert the in-progress mark,
   read the xref entry
2. **no borrow** — read and parse
3. short borrow — install into the canonical cache and the handle's slot,
   `updateCache` equivalent, drop the guard

Preserve what acceptance criterion 3 lists: cache identity, ObjGen, warnings,
recursion-loop fallback, teardown, and the exact parsed offset.

**Step 4:** `cargo test -p flpdf --lib`; then `cargo test --workspace` to prove
the legacy path is untouched.

**Step 5:** commit.

---

## Task 5: Re-entrancy regression — the point of the whole slice

**Files:** `crates/flpdf/src/reader/resolver.rs` tests

Build a document whose stream `/Length` is an indirect reference, so resolving
the stream re-enters the resolver mid-parse. This is the case
`QPDF::readStream` (`QPDF.cc:1360-1398`) brackets with
`stream_offset = m->file->tell()` … `m->file->seek(stream_offset, SEEK_SET)`.

**This test is what fails if a future edit holds a borrow across the seam.**
Write it so the failure mode is a clear panic message, and state in the comment
that a `RefCell` double-borrow — not a wrong value — is what it is guarding.

Prove it discriminates: move the borrow so it spans the nested resolution,
confirm this test panics, restore.

Also cover a self-referential `/Length` (the object's own length pointing at
itself), which is the case qpdf's loop guard exists for.

**Commit.**

---

## Task 6: `/AP /N` nested dereference — inherited from `flpdf-k8ln`

**Files:** `crates/flpdf/src/reader/resolver.rs` tests

`flpdf-k8ln` was folded into this issue; its acceptance criterion was that a
nested handle such as `/AP /N 5 0 R` resolves through the owning document.
Build an annotation whose `/AP /N` is an indirect stream and assert nested
`try_dereference` succeeds. Keep it uncompressed so it fits this slice.

**Commit.**

---

## Task 7: Verification

**REQUIRED SUB-SKILL:** superpowers:verification-before-completion

```
cargo fmt --all -- --check
cargo test -p flpdf --lib
cargo test --release -p flpdf --lib
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Changed-line coverage must be 100%. Clear `scripts/__pycache__` first if the
module-doc unittest was run — it dirties the tree and the gate correctly
refuses — write the report under `target/`, and never run two `llvm-cov`
invocations at once; they collide on the target directory and crash.

```
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path target/lcov-slice1.info
scripts/patch-coverage.sh --base origin/main
```

Record in `bd`: the classes this slice does **not** yet resolve, so the next
slice starts from an accurate boundary rather than re-deriving it.

Do not claim `flpdf-25kg.3.5` complete. This slice closes none of its
acceptance criteria on its own; criterion 2 explicitly requires every source
class, and criterion 5 gates `Pdf::get_object` publication behind that.
