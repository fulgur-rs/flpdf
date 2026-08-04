# flpdf-txag: linearize + encrypt support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `flpdf`'s linearized writer support encrypted output
(`WriteOptions::encrypt` / `WriteOptions::copy_encryption`) with a
non-deterministic (default or `--static-id`) `/ID`, byte-identical to qpdf
11.9.0, instead of the current behaviour of either erroring (when
`deterministic_id` is also set) or silently emitting **plaintext** (the
security bug this closes).

**Architecture:** Reuse the existing non-linearized full-rewrite encryption
machinery (`EncryptionContext`, `build_encryption_context`,
`encrypt_strings_in_object_for_writer`, `encrypt_stream_payload_for_writer` in
`crates/flpdf/src/writer.rs`) rather than reimplementing crypto. Extend
`RenumberMap` (`crates/flpdf/src/linearization/renumber.rs`) with one more
reserved sentinel slot for the `/Encrypt` dictionary, positioned exactly where
qpdf puts it (`QPDFWriter.cc:2563-2624`: between the catalog/open-document
objects and the hint stream). Hook per-object encryption into the linearized
writer's single serialization choke point (`append_object` /
`append_body_object` in `crates/flpdf/src/linearization/writer.rs`), plus the
hint-stream emitter. Scope: classic (stream-free) linearized layout only;
ObjStm + encrypt + linearize is out of scope and rejected with
`Unsupported`. `deterministic_id + encrypting` stays rejected (qpdf itself
throws for the same combination — verified empirically, see Task 5).

**Tech Stack:** Rust, `flpdf`/`flpdf-cli` crates. qpdf 11.9.0 reference source
is cached locally at `~/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDFWriter.cc` — read
the cited line ranges before writing code for tasks that touch qpdf parity.

**Design reference:** Full design rationale is saved on beads issue
`flpdf-txag` (`bd show flpdf-txag`) — read it before starting if this plan
alone leaves a decision ambiguous.

---

## Before you start

Read these in full before touching code (design-patterns rule 1: qpdf first,
flpdf structure second):

1. `~/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDFWriter.cc` lines 1159-1236
   (`writeTrailer` — `/Encrypt` only in the non-`t_lin_second` branch),
   2036-2184 (`doWriteSetup`/`prepareFileForWrite`), 2560-2905
   (`writeLinearized`, especially the object-number-sequence comment at
   2563-2624 and the `writeEncryptionDictionary()` call at line 2794-2796),
   2243-2256 (`writeEncryptionDictionary` — no `setDataKey` call, the dict
   itself is never encrypted), 2287-2299 (`writeHintStream` — **does** call
   `setDataKey(hint_id)`, the hint stream **is** encrypted), 590-648, 843-847
   (`setEncryptionParameters`/`setDataKey`), 1823-1909 (`generateID`,
   idempotent once `id2` is set).
2. `crates/flpdf/src/linearization/renumber.rs` lines 1-100 (module doc,
   `RenumberMap` layout) and the `from_plan` function (lines 191-350).
3. `crates/flpdf/src/writer.rs` lines 2237-2480 (`EncryptionContext`,
   `build_encryption_context`, `build_copy_encryption_context`), 2695-2860
   (`apply_encrypt_trailer_entries`, `encrypt_strings_in_object_for_writer`,
   `encrypt_stream_payload_for_writer`), 3660-3830 (the emission loop that
   calls those two functions — this is the pattern to replicate).
4. `crates/flpdf/src/linearization/writer.rs` lines 544-623 (`append_object`
   / `append_body_object` — the single choke point), 957-1017
   (`finalize_linearized_id`), 2541-2620 (`write_linearized` header and the
   existing `deterministic_id && encrypting` guard).

---

### Task 1: Widen visibility of the shared encryption machinery

**Files:**
- Modify: `crates/flpdf/src/writer.rs:2237-2480, 2738-2860`

**Step 1**

No new test — this is a pure visibility change (private `fn`/`struct` →
`pub(crate)`), so it cannot change any output byte on its own. Confirm this
by running the existing test suite before touching anything:

Run: `cargo test -p flpdf --lib writer:: 2>&1 | tail -5`
Expected: all passing (record the pass count to diff against after the
change).

**Step 2: Widen visibility**

Change these items in `crates/flpdf/src/writer.rs` from private to
`pub(crate)` (do not change field types, names, or logic):
- `enum WriteCipher` (line 2237) and its variants
- `struct EncryptionContext` (line 2247) and every field
- `fn build_encryption_context` (line 2292)
- `fn build_copy_encryption_context` (line 2455)
- `fn encrypt_strings_in_object_for_writer` (line 2742)
- `fn encrypt_stream_payload_for_writer` (line 2821)

**Step 3: Verify it still compiles and tests are unaffected**

Run: `cargo build -p flpdf 2>&1 | tail -20`
Expected: no errors, no new warnings about unused `pub(crate)` items (they're
about to be used by Task 5+).

Run: `cargo test -p flpdf --lib writer:: 2>&1 | tail -5`
Expected: identical pass count to Step 1.

**Step 4: Commit**

```bash
git add crates/flpdf/src/writer.rs
git commit -m "refactor(writer): widen encryption context visibility to pub(crate)

Pure visibility change (private -> pub(crate)) so the linearization module
can reuse the existing EncryptionContext machinery instead of duplicating
it. No behavior change; no output byte can be affected by a visibility
modifier alone."
```

---

### Task 2: `RenumberMap::reserve_encrypt_dict_slot`

**Files:**
- Modify: `crates/flpdf/src/linearization/renumber.rs`
- Test: same file, `#[cfg(test)] mod tests`

qpdf's object-number sequence for a linearized+encrypted file
(`QPDFWriter.cc:2563-2624`) inserts the `/Encrypt` dictionary's slot
immediately **before** the hint-stream slot (i.e. right after the
catalog/open-document-plain objects). `RenumberMap` already has this exact
kind of "reserved sentinel with no source object" for `param_dict_slot` and
`hint_stream_slot` — model the new slot the same way, added as a **post-hoc
transform** (matching the existing `place_objstm_members_per_half` pattern of
rebuilding the table after `from_plan`), not by changing `from_plan`'s
signature (which would force every existing non-encrypting caller to change).

**Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `renumber.rs`:

```rust
#[test]
fn reserve_encrypt_dict_slot_inserts_before_hint_and_shifts_it() {
    let plan = two_page_plan();
    let mut rn = RenumberMap::from_plan(&plan);
    let old_hint = rn.hint_stream_slot();

    let encrypt_ref = rn.reserve_encrypt_dict_slot();

    assert_eq!(encrypt_ref, ObjectRef::new(old_hint, 0));
    assert_eq!(rn.hint_stream_slot(), old_hint + 1);
}

#[test]
fn reserve_encrypt_dict_slot_shifts_every_object_at_or_after_old_hint_slot() {
    let plan = two_page_plan();
    let mut rn = RenumberMap::from_plan(&plan);
    // Snapshot every mapping before the reservation.
    let before: Vec<(ObjectRef, ObjectRef)> = rn.iter_in_layout_order().collect();
    let old_hint = rn.hint_stream_slot();

    rn.reserve_encrypt_dict_slot();

    for (old_new_ref, original) in before {
        let expected = if old_new_ref.number >= old_hint {
            ObjectRef::new(old_new_ref.number + 1, 0)
        } else {
            old_new_ref
        };
        assert_eq!(
            rn.new_for_original(original),
            Some(expected),
            "original {original:?} did not shift correctly"
        );
    }
}

#[test]
fn reserve_encrypt_dict_slot_leaves_param_dict_slot_untouched() {
    // param_dict_slot is always allocated before hint_stream_slot in
    // from_plan, so inserting immediately before the hint slot must never
    // shift the param dict slot.
    let plan = two_page_plan();
    let mut rn = RenumberMap::from_plan(&plan);
    let old_param = rn.param_dict_ref();

    rn.reserve_encrypt_dict_slot();

    assert_eq!(rn.param_dict_ref(), old_param);
}

#[test]
fn reserve_encrypt_dict_slot_is_reserved_not_original_for_new() {
    let plan = two_page_plan();
    let mut rn = RenumberMap::from_plan(&plan);
    let encrypt_ref = rn.reserve_encrypt_dict_slot();

    // The slot has no original object, same as param dict / hint stream.
    assert_eq!(rn.original_for_new(encrypt_ref), None);
}

#[test]
fn reserve_encrypt_dict_slot_len_increases_by_one() {
    let plan = two_page_plan();
    let mut rn = RenumberMap::from_plan(&plan);
    let before_len = rn.len();

    rn.reserve_encrypt_dict_slot();

    assert_eq!(rn.len(), before_len + 1);
}
```

**Step 2: Run to verify they fail**

Run: `cargo test -p flpdf --lib linearization::renumber:: 2>&1 | tail -20`
Expected: FAIL with "no method named `reserve_encrypt_dict_slot`".

**Step 3: Implement**

Add to `impl RenumberMap` in `renumber.rs`, near `place_objstm_members_per_half`:

```rust
/// Reserve a new sentinel slot for the `/Encrypt` dictionary object,
/// inserted immediately before the hint-stream slot — matching qpdf's
/// object-number sequence for linearized+encrypted output
/// (`QPDFWriter.cc:2563-2624`): `... catalog/open-document objects ->
/// encryption dictionary -> hint stream -> part6 ...`. Every already-
/// assigned slot at or after the old hint-stream position shifts up by
/// one; `param_dict_slot` is always allocated strictly before
/// `hint_stream_slot` in [`from_plan`](Self::from_plan) so it never shifts.
///
/// Must be called (when encrypting) immediately after `from_plan` and
/// before `place_objstm_members_per_half` — encrypted ObjStm-relocated
/// linearized output is out of scope (see `write_linearized`'s guard) and
/// this method does not attempt to preserve `place_objstm_members_per_half`
/// invariants.
///
/// Returns the newly reserved [`ObjectRef`] (generation 0) for the writer
/// to use as the `/Encrypt` object number.
pub(crate) fn reserve_encrypt_dict_slot(&mut self) -> ObjectRef {
    let insert_at = self.hint_stream_slot as usize;
    debug_assert!(
        (self.param_dict_slot as usize) < insert_at,
        "param_dict_slot must precede hint_stream_slot (from_plan invariant)"
    );
    self.by_new_number.insert(insert_at, SENTINEL);
    for new_ref in self.by_original.values_mut() {
        if new_ref.number as usize >= insert_at {
            new_ref.number += 1;
        }
    }
    self.hint_stream_slot += 1;
    ObjectRef::new(insert_at as u32, 0)
}
```

**Step 4: Run to verify they pass**

Run: `cargo test -p flpdf --lib linearization::renumber:: 2>&1 | tail -20`
Expected: PASS, all tests including the 5 new ones.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/renumber.rs
git commit -m "feat(linearization): add RenumberMap::reserve_encrypt_dict_slot

Reserves a sentinel object-number slot for the /Encrypt dictionary,
positioned exactly where qpdf's writeLinearized places it: between the
catalog/open-document objects and the hint stream (QPDFWriter.cc:2563-
2624). Modeled as a post-hoc rebuild, matching the existing
place_objstm_members_per_half pattern, so from_plan's signature (and every
non-encrypting caller) is untouched."
```

---

### Task 3: Guard ObjStm + encrypt + linearize as `Unsupported`

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` (inside `write_linearized`,
  before any call that would invoke `place_objstm_members_per_half`)
- Test: same file, `#[cfg(test)] mod tests`

**Step 1: Write the failing test**

Find an existing test that constructs an encrypting `WriteOptions` (e.g.
`deterministic_id_linearized_rejects_encrypt`) and an existing test/helper
that produces ObjStm-bearing linearized output (search for
`object_streams: ObjectStreamMode::Generate` or similar in this file's
tests) to build a fixture that combines both. Add:

```rust
#[test]
fn objstm_encrypt_linearize_combination_is_unsupported() {
    let src = /* a small multi-page fixture that reliably produces ObjStm
                 batches under Generate mode — reuse whatever helper the
                 existing ObjStm linearization tests use */;
    let err = linearize_with(&src, |o| {
        o.object_streams = ObjectStreamMode::Generate;
        o.encrypt = Some(/* a minimal EncryptParams, e.g. V4Aes128 with
                             empty passwords — reuse the helper the
                             non-linearized encrypt tests use */);
    })
    .unwrap_err();
    assert!(matches!(err, crate::Error::Unsupported(_)));
}
```

(Adapt the exact fixture/helper names to what's already in this file — grep
for `ObjectStreamMode::Generate` and `EncryptParams` in this test module
first rather than inventing new helpers.)

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib linearization::writer::tests::objstm_encrypt_linearize 2>&1 | tail -20`
Expected: FAIL — currently either succeeds (wrong) or fails for an unrelated
reason.

**Step 3: Implement the guard**

In `write_linearized`, after determining `encrypting` (already computed by
the existing `deterministic_id && encrypting` guard) and after the plan/
ObjStm-batch computation determines whether any ObjStm batch is non-empty,
add:

```rust
if encrypting && /* any ObjStm batch non-empty — reuse whatever boolean or
                     check the existing code already computes for this,
                     e.g. `!objstm_layout.is_empty()` */ {
    return Err(crate::Error::Unsupported(
        "linearize+encrypt does not yet support object streams; use \
         --object-streams=disable with --linearize --encrypt, or file a \
         follow-up if you need both".to_string(),
    ));
}
```

Place this **before** any call to `place_objstm_members_per_half` so the
ObjStm relocation path never has to reason about encryption.

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib linearization::writer::tests::objstm_encrypt_linearize 2>&1 | tail -20`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "feat(linearization): reject object-streams + encrypt + linearize

Out of scope for this pass (flpdf-txag): only the classic stream-free
linearized layout supports encryption. Reject explicitly rather than
silently mis-encrypting or silently dropping the ObjStm request."
```

---

### Task 4: Compute `/ID` before renumbering; widen the encrypting guard

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs:2541-2620` and the body of
  `write_linearized` where `finalize_linearized_id` / `RenumberMap::from_plan`
  are currently called

**Step 1: Read current call order**

Before writing any code, grep `write_linearized`'s body for
`finalize_linearized_id` and `RenumberMap::from_plan` and note their current
relative order. If `finalize_linearized_id` is already called before
`RenumberMap::from_plan`, this task only widens the guard (Step 3) and
threads the id through (Step 4) — no reordering needed. If it's called
later, move the `finalize_linearized_id` call (and whatever it depends on:
`source_trailer`, `det_id_source_id0`) earlier, immediately after the
existing `deterministic_id && static_id` guard and before `RenumberMap::from_plan`.

**Step 2: Write a failing test pinning the widened guard's new behavior**

```rust
#[test]
fn non_deterministic_encrypt_linearize_no_longer_rejected_by_guard() {
    // This does not assert full success yet (later tasks implement that) —
    // it asserts the function does NOT return the
    // "deterministic-id option is incompatible with encrypted output"
    // Unsupported error for a NON-deterministic-id encrypting request.
    // It's fine (expected, until Task 5+) for this to fail for some OTHER
    // reason (missing encrypt_dict_slot wiring, etc.) — this test only
    // pins the guard message, so assert on the error message content
    // rather than the whole Result.
    let src = /* minimal one-page fixture, matches existing tests' style */;
    let result = linearize_with(&src, |o| {
        o.encrypt = Some(/* minimal V4Aes128 EncryptParams */);
        // o.deterministic_id left at its default `false`
    });
    if let Err(crate::Error::Unsupported(msg)) = &result {
        assert!(
            !msg.contains("deterministic-id option is incompatible"),
            "non-deterministic-id encrypting must not hit the deterministic-id guard: {msg}"
        );
    }
}
```

**Step 3: Widen the guard**

The current guard (lines ~2565-2570) is:

```rust
let encrypting = options.encrypt.is_some() || options.copy_encryption.is_some();
if options.deterministic_id && encrypting {
    return Err(crate::Error::Unsupported(
        "the deterministic-id option is incompatible with encrypted output files".to_string(),
    ));
}
```

This guard is already correctly scoped (`deterministic_id && encrypting`,
not `encrypting` alone) — it does **not** need widening to reject more
cases; it already matches qpdf's real behavior (verified empirically: `qpdf
--deterministic-id --linearize --encrypt ...` fails with the same
"generateID has no data for deterministic ID" reasoning). Update only the
doc comment above it (and the `write_linearized` rustdoc `# Errors` section
around lines 2395-2401) to state that qpdf *does* support
non-deterministic-id linearize+encrypt, so a future reader does not mistake
this guard for "linearize+encrypt is unsupported" — cite the empirical qpdf
test in the comment (`qpdf --linearize --encrypt "" "" 128 --use-aes=y --`
succeeds; `qpdf --deterministic-id --linearize --encrypt ...` fails with
qpdf's own `QPDFWriter::generateID has no data for deterministic ID`
internal error).

**Step 4: Thread the early `/ID` into `write_linearized`**

Ensure `finalize_linearized_id` (or the value it returns) is available
before `RenumberMap::from_plan` is called, so Task 5 can pass `/ID[0]` into
`build_encryption_context`. If `finalize_linearized_id` returns an
`Object::Array` of two `Object::String`s, extract the first element's bytes
as `id0: &[u8]` for Task 5.

**Step 5: Run to verify**

Run: `cargo test -p flpdf --lib linearization::writer:: 2>&1 | tail -30`
Expected: all existing tests still pass; the new guard-message test passes.

**Step 6: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "refactor(linearization): compute /ID before renumbering

Prerequisite for encryption support: the file encryption key derives from
/ID[0] (PDF Algorithm 2), and qpdf computes /ID once, early, via
generateID()'s idempotent guard. flpdf's non-deterministic and --static-id
/ID computation is already content-independent, so it can move earlier
without changing any output byte for existing (non-encrypting) callers.
Also clarifies the existing deterministic_id && encrypting guard's doc:
qpdf DOES support the non-deterministic-id combination (verified against
qpdf 11.9.0 empirically)."
```

---

### Task 5: Build the `EncryptionContext` and reserve the slot in `write_linearized`

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn linearize_with_encrypt_reserves_encrypt_dict_object_and_succeeds() {
    let src = /* minimal one-page fixture */;
    let doc = linearize_with(&src, |o| {
        o.encrypt = Some(/* minimal V4Aes128 EncryptParams, matching the
                             helper the non-linearized encrypt tests use */);
    })
    .expect("linearize+encrypt with non-deterministic id must now succeed");

    // The output must contain an indirect object whose dict has
    // /Filter /Standard (the /Encrypt dict qpdf always includes).
    let bytes = &doc.bytes;
    assert!(
        find_subslice(bytes, b"/Filter /Standard").is_some(),
        "expected an /Encrypt dictionary with /Filter /Standard in the output"
    );
}
```

(Use whatever byte-search helper already exists in this test module — grep
for existing `bytes.windows(` or similar patterns rather than inventing a
new `find_subslice`.)

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib linearization::writer::tests::linearize_with_encrypt_reserves 2>&1 | tail -20`
Expected: FAIL (no `/Encrypt` dict emitted yet).

**Step 3: Implement**

In `write_linearized`, after the `/ID` is finalized (Task 4) and before
`RenumberMap::from_plan` is called:

```rust
use crate::writer::{build_copy_encryption_context, build_encryption_context, EncryptionContext};

let encrypt_ctx: Option<EncryptionContext> = if let Some(params) = &options.encrypt {
    Some(build_encryption_context(pdf, options, /* metadata_ref, existing_max — read
        build_encryption_context's exact signature in writer.rs:2292 and match it */, params, &id0)?)
} else if let Some(src) = &options.copy_encryption {
    Some(build_copy_encryption_context(src, options, /* existing_max */)?)
} else {
    None
};
```

Read `build_encryption_context`'s and `build_copy_encryption_context`'s
exact parameter lists in `writer.rs` (lines 2292 and 2455) — do not guess;
the signature shown above is illustrative, not exact. Note both functions
take `existing_max: u32` (the highest object number before the encrypt dict
is allocated) — for the linearized path this is `renumber_map.len()` *before*
`reserve_encrypt_dict_slot()` is called, since `build_encryption_context`
internally does `existing_max.checked_add(1)` to pick the encrypt object
number, and Task 2's method must be the one that actually wins that number
(qpdf's mid-sequence placement, not "max + 1" — the standard-writer's
`existing_max + 1` scheme does NOT apply here). If `build_encryption_context`
hard-codes `encrypt_ref = ObjectRef::new(existing_max + 1, 0)` internally
in a way that can't be overridden, that field must be **overwritten** after
the call with the ref returned by `reserve_encrypt_dict_slot()` — check
whether `EncryptionContext.encrypt_ref` is a plain field you can reassign
after construction (it should be, since it's just data).

Then:

```rust
let renumber = RenumberMap::from_plan(&plan);
let mut renumber = renumber; // still needed mutably below regardless
let encrypt_ctx = encrypt_ctx.map(|mut ctx| {
    ctx.encrypt_ref = renumber.reserve_encrypt_dict_slot();
    ctx
});
```

(Adjust exact variable flow to match the surrounding code's existing
mutability patterns — read the ~30 lines around the current
`RenumberMap::from_plan` call first.)

**Step 4: Run to verify it still fails for the RIGHT reason**

Run: `cargo test -p flpdf --lib linearization::writer::tests::linearize_with_encrypt_reserves 2>&1 | tail -20`
Expected: still FAIL, but now because the `/Encrypt` object is never
*written* (Task 6/7), not because the slot/context isn't built. Confirm by
temporarily adding `eprintln!("{:?}", encrypt_ctx.is_some())` or by
inspecting with a debugger — remove any debug prints before committing.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "feat(linearization): build EncryptionContext and reserve its object slot

Wires the shared full-rewrite encryption context builder into
write_linearized, using the /ID[0] finalized in the previous commit. The
encrypt dict's object number comes from RenumberMap::reserve_encrypt_dict_slot
(qpdf's mid-sequence placement), overriding whatever number
build_encryption_context's existing_max+1 default assigned. The dict is not
yet emitted to output bytes — that's the next commit."
```

---

### Task 6: Emit the `/Encrypt` dictionary object in the body

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs` (`do_write_pass`, right
  after the catalog + `part4_open_document_plain` objects are written, before
  the hint stream — mirrors `QPDFWriter.cc:2793-2803`'s
  `if (cur_object.getObjectID() == part4_end_marker) { if (m->encrypted) {
  writeEncryptionDictionary(); } ... }`)

**Step 1: Locate the exact insertion point**

In `do_write_pass`, find where the catalog and
`objstm_layout`/`plan.part4_open_document_plain` objects finish emitting and
the hint stream is about to be written (search for where
`hint_stream_new_num` / the hint-stream slot is consumed, and for the loop
that emits `renumber.iter_in_layout_order()` or equivalent body-object
emission before that point).

**Step 2: Write the failing test (extends Task 5's test)**

Strengthen `linearize_with_encrypt_reserves_encrypt_dict_object_and_succeeds`
(or add a new test) to assert the `/Encrypt` object's number matches
`encrypt_ctx.encrypt_ref.number` exactly, e.g. by searching for
`b"{N} 0 obj"` where `N` is that number, immediately followed (within the
object body) by `/Filter /Standard`.

**Step 3: Implement**

At the insertion point, when `encrypt_ctx` is `Some(ctx)`, serialize the
`/Encrypt` dictionary as a plain (non-encrypted — see the "Before you start"
note: `writeEncryptionDictionary` never calls `setDataKey`) indirect object:

```rust
if let Some(ctx) = &encrypt_ctx {
    append_object(&mut bytes, ctx.encrypt_ref, &Object::Dictionary(ctx.encrypt_dict.clone()));
    // record its offset the same way every other body object's offset is
    // recorded in this function (xref_offsets.insert(...)), matching the
    // existing pattern used for other plain objects in this loop.
}
```

(`ctx.encrypt_dict`'s exact field name/type — confirm against the
`EncryptionContext` struct definition from Task 1/5; it should already be a
`Dictionary` ready to serialize, matching how the non-linearized writer
inserts it via `trailer.insert("Encrypt", ...)` after building it — but here
we're serializing the dict *object itself*, not just referencing it.)

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib linearization::writer::tests::linearize_with_encrypt_reserves 2>&1 | tail -20`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "feat(linearization): emit the /Encrypt dictionary object

Serialized as a plain (unencrypted) indirect object immediately after the
catalog/open-document objects, before the hint stream — matching qpdf's
writeLinearized insertion point (QPDFWriter.cc:2793-2796) and its
writeEncryptionDictionary, which never applies a per-object data key (the
dict must be readable before the key can be derived)."
```

---

### Task 7: `/Encrypt <ref> 0 R` in the first-half trailer only

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`
  (`write_part1_xref_and_trailer` for the classic path,
  `write_first_page_xref_stream` for the xref-stream path — **not**
  `write_main_xref_and_trailer` / `write_main_xref_stream_and_trailer`)

Since Task 3 rejects the ObjStm/xref-stream form when encrypting, only
`write_part1_xref_and_trailer` needs the change for this issue's shipped
scope — but add the same trailer key to `write_first_page_xref_stream` too
if it's cheap and clearly scoped (guarded by `encrypt_ctx.is_some()`, which
Task 3 guarantees is only reachable together with the classic path); if it
adds meaningful risk/complexity, skip it and leave a `// TODO` — but per
CLAUDE.md, doc TODOs are not allowed in public rustdoc, so use a plain `//`
comment, not `///`, and note the scope boundary in the function's existing
non-public comments instead.

**Step 1: Write the failing test**

Extend the Task 5/6 test (or add a new one) to assert the first-half
trailer dict contains `/Encrypt {N} 0 R` where `N == encrypt_ctx.encrypt_ref.number`,
and that the **main** (second/last) trailer does **not** contain `/Encrypt`
at all — search for `b"/Encrypt"` and assert it appears exactly once in the
whole output.

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib linearization::writer::tests::linearize_with_encrypt 2>&1 | tail -20`
Expected: FAIL (no `/Encrypt` key in either trailer yet — only the object
itself exists from Task 6).

**Step 3: Implement**

In `write_part1_xref_and_trailer`, find where the trailer dict's keys are
written (it already writes `/Info`, `/Root`, `/Size`, `/Prev`, `/ID` per the
existing doc comment at lines 671-673) and add, when encrypting:

```rust
if let Some(ctx) = encrypt_ctx {
    // key order: qpdf writes /Encrypt last among the non-/ID trailer keys
    // (writeTrailer's key loop iterates the trailer dict's own key order,
    // then appends /ID, then /Encrypt after /ID — verify the exact final
    // key order against QPDFWriter.cc:1174-1231 and an actual qpdf output
    // sample, e.g. /tmp/out_lin_enc.pdf produced during design research,
    // rather than guessing).
}
```

Confirm the exact key order empirically before finalizing: run (if not
already available)
`qpdf --linearize --static-id --static-aes-iv --encrypt "" "" 128 --use-aes=y -- <any.pdf> /tmp/oracle.pdf`
and inspect the first-half trailer's raw bytes (`python3 -c "..."` dump or
`less -A /tmp/oracle.pdf`, cf. the `/ID … /Encrypt` order already observed
during design: `... /ID [<...><...>] /Encrypt 18 0 R >>` — `/Encrypt` comes
immediately after `/ID`, matching `writeTrailer`'s code order: keys loop,
then `/ID`, then `/Encrypt`).

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib linearization::writer::tests::linearize_with_encrypt 2>&1 | tail -20`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "feat(linearization): write /Encrypt in the first-half trailer only

Matches qpdf's writeTrailer (QPDFWriter.cc:1224-1231): the /Encrypt
reference is written for every trailer form except t_lin_second (the main/
second-half trailer). Key order is /ID then /Encrypt, verified against an
actual qpdf 11.9.0 --linearize --encrypt output sample."
```

---

### Task 8: Per-object string/stream encryption in the body writer

**Files:**
- Modify: `crates/flpdf/src/linearization/writer.rs`
  (`append_object`/`append_body_object`, plus the hint-stream emitter
  `append_hint_stream_object`)

**Step 1: Write the failing test**

```rust
#[test]
fn linearize_with_encrypt_body_strings_and_streams_are_ciphertext() {
    let src = /* fixture with at least one string in a dict (e.g. /Producer
                 in /Info, if the fixture builder sets one) and one content
                 stream with recognizable plaintext bytes (e.g. "BT /F1" or
                 similar — check what the existing fixture helpers put in
                 page content streams) */;
    let doc = linearize_with(&src, |o| {
        o.encrypt = Some(/* minimal V4Aes128 EncryptParams */);
    })
    .unwrap();

    // The known plaintext content-stream bytes must NOT appear verbatim in
    // the encrypted output (they would if encryption weren't applied).
    assert!(
        find_subslice(&doc.bytes, b"<the known plaintext marker>").is_none(),
        "content stream plaintext leaked into encrypted output"
    );
}

#[test]
fn linearize_with_encrypt_xref_stream_is_not_encrypted() {
    // Only reachable if Task 3's guard is scoped correctly to allow the
    // classic (non-ObjStm) form with encryption — this test targets the
    // CLASSIC xref table, not an xref stream (Task 3 rejects
    // ObjStm+encrypt, and the classic path never emits an xref *stream*).
    // If there is no separate xref-stream-without-ObjStm code path in this
    // writer, skip/delete this test — confirm by reading how
    // `need_xref_stream` (or flpdf's equivalent) is derived before writing
    // it.
}
```

Skip or adapt the second test based on what Task 3's investigation reveals
about whether a stream-free document can still trigger flpdf's xref-*stream*
form (some PDF 1.5+ documents use xref streams even without ObjStm) — if so,
Task 3's guard scope needs re-checking: it should key off "does the
*trailer* form emitted require an xref stream", not merely "are there ObjStm
batches", to decide whether encryption is supported. Re-read the design
note in `bd show flpdf-txag` if this is ambiguous, and prefer the more
conservative (reject more, ship less) interpretation over guessing.

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib linearization::writer::tests::linearize_with_encrypt_body 2>&1 | tail -20`
Expected: FAIL (plaintext still visible — no per-object encryption applied
yet).

**Step 3: Implement**

In `append_body_object` (and `append_object` for non-stream objects), add an
`encrypt_ctx: Option<&EncryptionContext>` parameter (thread it through every
call site — there should be few, inside `do_write_pass`). Before writing the
object:

```rust
let mut object = object.clone(); // now owned/mutable, matching the
                                   // non-linearized emission loop's pattern
if let Some(ctx) = encrypt_ctx {
    if new_ref != ctx.encrypt_ref {
        crate::writer::encrypt_strings_in_object_for_writer(new_ref, &mut object, ctx)?;
    }
}
```

For the stream-payload branch (after `reencode_stream_for_compress`,
matching the non-linearized writer's ordering at `writer.rs:3801-3809`):

```rust
if let Some(ctx) = encrypt_ctx {
    if new_ref != ctx.encrypt_ref {
        if let Object::Stream(ref mut s) = reencoded {
            crate::writer::encrypt_stream_payload_for_writer(new_ref, s, ctx)?;
        }
    }
}
```

Both `append_object`/`append_body_object` currently return `usize`
(offset), not `Result<usize>` — check whether `encrypt_stream_payload_for_writer`
/`encrypt_strings_in_object_for_writer` can fail (they return `Result<()>`
per the "Before you start" reading) and propagate the error by changing the
return type to `Result<usize>` if needed. Update every call site
accordingly.

For the hint stream (`append_hint_stream_object`), apply
`encrypt_stream_payload_for_writer` to its payload the same way — the hint
stream **is** encrypted (confirmed: `writeHintStream` calls
`setDataKey(hint_id)` in qpdf). Use `renumber.hint_stream_slot()` (post-Task-2
shift) as the object ref for key derivation.

Do **not** touch any xref-table or xref-stream writer function — those must
stay unencrypted (`cur_data_key.clear()` in qpdf).

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib linearization::writer:: 2>&1 | tail -40`
Expected: all existing + new tests pass. Pay particular attention to any
existing non-encrypting test whose expected bytes might shift if the
`encrypt_ctx: Option<&EncryptionContext>` threading accidentally changed
behavior for `None` — it must be a no-op when `encrypt_ctx` is `None`.

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/writer.rs
git commit -m "feat(linearization): encrypt body-object strings/streams and the hint stream

Hooks encrypt_strings_in_object_for_writer / encrypt_stream_payload_for_writer
into append_object/append_body_object (the writer's single serialization
choke point, shared by every convergence-loop iteration and the final
write) and into the hint-stream emitter. The /Encrypt dict object and xref
table/stream stay unencrypted, matching qpdf's setDataKey/cur_data_key.clear
pattern verified in QPDFWriter.cc."
```

---

### Task 9: CLI — allow `--linearize` with `--encrypt`/`--copy-encryption-from`

**Files:**
- Modify: `crates/flpdf-cli/src/main.rs` (lines ~553-559 and ~578-584 for the
  `conflicts_with_all` lists; check lines ~949-975 too — there appear to be
  two near-duplicate `--encrypt`/`--copy-encryption-from` definitions, one
  top-level and one on the `rewrite` subcommand)
- Test: wherever this crate's existing CLI arg-parsing tests live (grep for
  existing `conflicts_with` regression tests, e.g. around the `--qdf
  --linearize` rejection test, for the pattern to follow)

**Step 1: Write the failing test**

Add a CLI-level test asserting `flpdf rewrite --linearize --encrypt "" "" 128
--use-aes=y -- in.pdf out.pdf` (using a real small fixture PDF) exits 0 and
produces a linearized, encrypted output — reuse whatever existing test
harness already invokes the CLI binary as a subprocess (grep for
`Command::new` or similar in this crate's test files) rather than inventing
a new one. Also add/keep a regression test that `--qdf --linearize` still
fails (must NOT be affected by this change).

**Step 2: Run to verify it fails**

Expected: FAIL with the current
`error: the argument '--linearize' cannot be used with '--encrypt ...'`
clap error.

**Step 3: Implement**

Remove `"linearize"` from both `conflicts_with_all` lists (top-level and
`rewrite` subcommand, both `--encrypt` and `--copy-encryption-from`). Do
**not** remove `"qdf"` from either list. Then verify (read, don't guess)
whether the `run_rewrite`/top-level `--linearize` dispatch branch(es)
already thread `options.encrypt`/`options.copy_encryption` into the
`WriteOptions` passed to `write_linearized` — if the CLI already builds one
shared `WriteOptions` before dispatching to either `write_pdf`/full-rewrite
or `write_linearized`, this may already work once the clap restriction is
lifted. If the linearize branch currently constructs a *separate*,
narrower `WriteOptions` that omits `encrypt`/`copy_encryption`, add those
fields.

**Step 4: Run to verify it passes**

Run the new CLI test; also manually smoke-test:

```bash
cd crates/flpdf-cli && cargo run -- rewrite --linearize --encrypt "" "" 128 --use-aes=y -- \
  <a small fixture pdf under tests/fixtures or similar> /tmp/flpdf_lin_enc_smoke.pdf
qpdf --check /tmp/flpdf_lin_enc_smoke.pdf
```

Expected: exit 0, `qpdf --check` reports no errors and "File is linearized".

**Step 5: Commit**

```bash
git add crates/flpdf-cli/src/main.rs
git commit -m "fix(cli): allow --linearize with --encrypt / --copy-encryption-from

qpdf itself supports this combination (verified: 'qpdf --linearize
--encrypt \"\" \"\" 128 --use-aes=y --' produces a valid, qpdf --check-clean
linearized encrypted file). The prior conflicts_with_all rejection predated
library-level support and is no longer accurate; --qdf stays excluded
(qpdf forces qdf_mode=false when linearized, so the two are inherently
incompatible regardless of encryption)."
```

---

### Task 10: Byte-identical oracle test

**Files:**
- Modify/Create: wherever existing `qpdf-zlib-compat`-gated byte-identical
  CLI tests live (grep for `cli_byte_identical` or similar helper used by
  the AES-128 whole-document test mentioned in recent git history —
  `e1f2502f test(cli): add whole-document qpdf byte-parity test for
  AES-128 encryption` — follow that test's exact structure)
- Modify: `.github/workflows/ci.yml` — add the new test to the explicit
  `qpdf-zlib-compat` test list (per this repo's established gotcha: a
  feature-gated test not explicitly listed in `ci.yml` silently does not
  run in CI)

**Step 1: Locate the precedent test**

Read the test added in commit `e1f2502f` (`git show e1f2502f`) in full —
this is the closest existing precedent (AES-128 byte-parity for the
non-linearized path) and this task's test should follow the same shape:
build/locate a fixture PDF, shell out to the real `qpdf` binary with
`--static-id --static-aes-iv`, shell out to `flpdf` with the equivalent
flags, and `assert_eq!` the two output byte vectors (or a helper that does
so with a useful diff on failure).

**Step 2: Write the test**

```rust
#[test]
#[cfg(feature = "qpdf-zlib-compat")]
fn cli_linearize_encrypt_aes128_byte_identical_to_qpdf() {
    // Follow e1f2502f's precedent exactly for fixture selection, qpdf
    // invocation, and the byte-diff assertion helper. New flags vs. that
    // precedent: --linearize, --static-aes-iv (both flpdf and qpdf sides).
}
```

**Step 3: Run and iterate**

Run: `cargo test -p flpdf-cli --features qpdf-zlib-compat cli_linearize_encrypt_aes128 2>&1 | tail -60`

If it fails with a byte mismatch, **do not guess-fix**. Per the project's
`flpdf-qpdf-review-oracle` skill and existing memory
(`behavior-changing-fix-needs-qpdf-oracle-check`), find the first differing
byte offset, correlate it against `qpdf --show-xref` output for both files
and the qpdf source line that produces that byte, and fix the actual root
cause (likely in Tasks 6-8's implementation) rather than adjusting the test.
Common first-diff culprits for this feature specifically, in likely order:
trailer key order (Task 7), which objects get a data key vs. not (Task 8 —
double check the hint stream IS encrypted and the xref table/`/Encrypt`
dict are NOT), or the encrypt dict's object number (Task 2/5).

**Step 4: Add to CI**

Edit `.github/workflows/ci.yml`: find the `qpdf-zlib-compat`-gated test
invocation step and add this test's fully-qualified name to whatever
explicit list/pattern already gates that suite in CI (per this repo's
established convention — grep the workflow file for `qpdf-zlib-compat` and
existing test names like the one from `e1f2502f` to find the right spot).

**Step 5: Run once more locally to confirm green, then commit**

```bash
git add crates/flpdf-cli/tests/ .github/workflows/ci.yml # exact paths per Step 1's findings
git commit -m "test(cli): add whole-document qpdf byte-parity test for linearize+encrypt

Follows the AES-128 byte-parity precedent (e1f2502f) with --linearize and
--static-aes-iv added. Registered explicitly in ci.yml (qpdf-zlib-compat
feature-gated tests do not run in CI unless listed)."
```

---

### Task 11: Additional method coverage (V5R6Aes256 / V5R5Aes256) + qualitative checks

**Files:**
- Modify: same test locations as Tasks 8 and 10

**Step 1**

Add a unit test (in `linearization/writer.rs`, following Task 8's pattern)
exercising `EncryptMethod::V5R6Aes256` through `linearize_with`, asserting
success and that the `/Encrypt` dict contains `/V 5` and `/CFM /AESV3` (or
however the existing non-linearized V5 tests assert this — mirror that
assertion style).

**Step 2**

Add the qualitative "hint stream is ciphertext, xref table is plaintext"
check called out in the design (`bd show flpdf-txag`'s acceptance
criteria item 6): locate the hint stream's raw bytes in the *unencrypted*
control output (same fixture, no `encrypt` option) vs. the *encrypted*
output at the equivalent structural position, and assert they differ
(ciphertext), while asserting the classic xref table's `%d %d %s \n`-style
entries are still ASCII-parseable (plaintext) in the encrypted output.

**Step 3: Run everything, commit**

```bash
cargo test -p flpdf --lib linearization:: 2>&1 | tail -10
cargo test -p flpdf-cli 2>&1 | tail -10
git add -A
git commit -m "test(linearization): cover V5R6Aes256/V5R5Aes256 and hint/xref encryption qualitative checks"
```

---

### Task 12: Patch coverage gate + final review

**Files:** none new — verification only.

**Step 1**

```bash
git add -A  # ensure everything from Tasks 1-11 is committed first
scripts/patch-coverage.sh --base main
```

Expected: `flpdf` crate changed lines at 100%; `flpdf-cli` changed lines
reported (not blocking). If any `flpdf` line is uncovered, either add a
test or annotate with `// cov:ignore: <reason>` per `CLAUDE.md`'s policy —
do not leave any changed line silently uncovered.

**Step 2: Qualitative check (CLAUDE.md gate item 4)**

Manually re-read every new/changed public-behavior branch and confirm a
real test exercises it: the `ObjStm+encrypt` `Unsupported` arm (Task 3),
the widened-but-unchanged `deterministic_id+encrypting` arm (Task 4, keep
its existing test), the CLI `conflicts_with_all` removal (Task 9), and the
byte-identical oracle (Task 10). Confirm none of these are only "line
executed" but assertion-free.

**Step 3**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace 2>&1 | tail -30
```

Fix anything red before proceeding.

**Step 4**

Update `flpdf-txag`'s beads notes with a one-line pointer to this plan and
mark ready for the finishing-a-development-branch step (PR creation) — do
not close the issue yet; that happens after PR review per this project's
session-completion protocol.
