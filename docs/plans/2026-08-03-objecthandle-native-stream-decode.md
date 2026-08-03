# ObjectHandle-native stream/filter decode primitives (flpdf-25kg.3.4)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add qpdf-native stream decode entry points that take an `ObjectHandle`
stream dictionary and produce decoded bytes, so `flpdf-25kg.3.5`
(`QPDF::resolve`/`resolveObjectsInStream`) can decode an object stream without
any raw `Object` conversion.

**Architecture:** Make the existing `&Object`-typed seam inside `filters.rs`
/`stream_filter.rs` **shape-neutral** (a `FilterSpec` carrying a bounded
`DecodeParams` value instead of `Option<&Object>`), then add a second *shape
reader* that builds the same `FilterSpec` list from an `ObjectHandle` using the
lazily-dereferencing `try_*` accessors landed by `flpdf-25kg.3.3`. The codec
stack, predictor geometry, limits, and warning-ordering engine stay as one
copy shared by both readers.

**Tech Stack:** Rust (workspace `flpdf`), pinned qpdf 11.9.0 oracle at
`scripts/fetch-qpdf-source.sh --print-path`, `cargo llvm-cov` +
`scripts/patch-coverage.sh` for changed-line coverage.

---

## Why this shape (read before Task 1)

Three facts from the codebase decide the architecture. Do not re-litigate them
mid-implementation; if one turns out false, stop and re-plan.

1. **`filters.rs` and `stream_filter.rs` are not deletion targets.**
   `grep -rn "qpdf-cutover-delete" --include=*.rs crates/flpdf/src` marks only
   `object_handle.rs`, `reader.rs`, `object.rs`, `ref_chain.rs` — the raw-Object
   graph and resolver routes. The codec layer is the ✅ `QPDFStreamFilter.cc` /
   `Pl_*` correspondence (`docs/qpdf-correspondence.md:188-198`). Therefore
   AC4's "existing legacy functions" means the `&Dictionary`-shaped **public
   entry points** (`decode_stream_data`, `decode_stream_data_with_limits`,
   `decode_stream_data_recovering*`, `encode_stream_data`), not every shared
   internal beneath them.

2. **The `&Object` seam is tiny and fully contained.** `decode_filter_specs`,
   `FilterSpec`, and `StreamFilter::set_decode_params` have callers only at
   `filters.rs:356`, `:520-560`, `:669-676`, `:700`. Nothing outside
   `filters.rs`/`stream_filter.rs` touches them.

3. **Forking below the seam would duplicate the parity-critical part.**
   `filters.rs` owns `PositionedDecodeEvent`, `position_pending_events`,
   `sort_positioned_events`, `append_final_crypt_events`,
   `replay_strict_decode_event` — the warning-order machinery AC3 requires to
   match qpdf. A second copy would have to stay byte-identical forever. One
   copy, two shape readers.

**AC4 "byte-identical" is read behaviorally**, not as source text: the legacy
entry points keep their signatures, their public tests
(`crates/flpdf/tests/stream_decode_recovery_public_api.rs`) are not edited, and
`scripts/qpdf-stream-codecs-diff.sh` still passes. Task 1's refactor changes
internal bodies only.

**Scope discipline.** This issue ships *primitives*. No consumer migration, no
`Pdf::get_object`, no `reader.rs` wiring — `.3.5`/`.3.6`/`.3.7` own those. The
new native functions may legitimately land with **zero production callers**;
that is AC4 ("gain no new callers") working as intended, not an incomplete
slice.

### Pinned qpdf oracle

`QPDF_Stream::filterable` (`libqpdf/QPDF_Stream.cc:379-484`) is the shape
authority. Its order is:

1. `stream_dict.getKey("/Filter")` — null → no filters; name → one; array →
   each item must be a name, else `filters_okay = false`; anything else →
   `filters_okay = false`.
2. `!filters_okay` → `warn("stream filter type is not name or array")`, return
   false.
3. Expand `filter_abbreviations`, look up `filter_factories`; unknown name →
   `filterable = false`, return false.
4. `getKey("/DecodeParms")`; empty array → treated as null; array → per-index;
   otherwise → replicated for every filter.
5. Length mismatch (only when `filters` is non-empty) →
   `warn("stream /DecodeParms length is inconsistent with filters")`,
   `filterable = false`.
6. Per filter, `filter->setDecodeParms(decode_item)`; false → `filterable = false`.

Every `getKey`/`getArrayItem` there is a `QPDFObjectHandle` accessor, which
auto-dereferences through the owning `QPDF`. That is why the native reader must
use `try_*`, not the non-resolving `as_*`.

### Decisions recorded up front

- **D1 — indirect `/Filter` / `/DecodeParms`.** The native reader uses the
  crate-private `try_dereference`/`try_get_key`/`try_as_dictionary` family
  (`object_handle.rs:507-602`) plus the new `try_as_*`/`try_array_len`
  accessors from Task 4, so an indirect child resolves through its document
  exactly as qpdf's accessors do. Do **not** add a non-resolving fallback.

  **Correction (2026-08-03, review of PR #626).** An earlier revision of this
  bullet said a handle "whose document was dropped" returns
  `Error::Internal("object N G belongs to a dropped PDF")`. That is only one of
  two states, and it is *not* the one production teardown produces:

  - `Error::Internal` comes from `try_dereference` on a handle that is still
    `NotYetResolved` and whose resolver `Weak` cannot be upgraded
    (`object_handle.rs`, `try_dereference`). Today that also covers every
    handle `Pdf::get_object_handle` hands out before it is resolved, because
    `new_indirect_unresolved_for_pdf` leaves `resolver: None` until
    `flpdf-25kg.3.5` wires the live link.
  - Production teardown is the other path: `impl Drop for Pdf`
    (`crates/flpdf/src/reader.rs:351-371`) calls `ObjectHandle::disconnect` on
    every registry entry, explicitly mirroring `QPDF::~QPDF`
    (`libqpdf/QPDF.cc:215-236`), which sets the slot to `Destroyed`.
    `try_dereference` returns `Ok(())` for every state other than
    `NotYetResolved`, so `Destroyed` is terminal, `with_value` presents it as
    `ObjectValue::Null` (`object_handle.rs:1344`), and `is_null` is
    `matches!(value, Some(ObjectValue::Null))` (`:815-817`). A `/Filter` handle
    disconnected this way therefore reads as **absent**, and
    `decode_filter_specs_from_handle` returns `Ok(vec![])` — no error at all.

  Pinned by `stream_filter`'s
  `handle_reader_reads_a_filter_disconnected_by_pdf_teardown_as_absent`, which
  builds a real `Pdf`, resolves the handle, drops the document, and reads it
  again. The existing `handle_reader_surfaces_a_dropped_document_from_every_\
  child_position` covers only the first bullet: it drops a *synthetic*
  resolver while its handle is still `NotYetResolved`, which teardown never
  does.

  The second bullet is a **genuine qpdf divergence**, filed as beads
  `flpdf-nrp3` (P2) and deliberately **not** fixed here. qpdf's `isNull()` is
  `dereference() && getTypeCode() == ::ot_null`
  (`libqpdf/QPDFObjectHandle.cc:353-356`) and `QPDF_Destroyed` is
  `::ot_destroyed`, so qpdf answers false where flpdf answers true. `is_null`
  is `pub`, its `Destroyed`-reads-as-null behavior is an approved
  `flpdf-25kg.3.3` contract with a test named for it
  (`disconnect_replaces_a_resolved_value_and_presents_as_null`), and changing
  it needs a sweep of every `is_null` caller. The divergence is `is_null`
  **alone** — `as_name`/`as_array`/`as_integer`/`as_dictionary` all fall
  through to `None` for `Destroyed`, which is what qpdf does for
  `::ot_destroyed` too.
- **D2 — Crypt.** The crypt provider stays an explicit closure parameter. Its
  only production instantiation today (`filters.rs:334-338`) returns
  `Error::Unsupported("unsupported stream filter: Crypt")`. The native path
  mirrors Crypt *selection* (recognising the stage and routing to the provider),
  not document-owned decryption. No `QPDF`-style document hookup here.
  `ParamValue::Name` exists so that provider can eventually read Crypt's
  `/Name` without widening this shared type during Phase 3's AES/Crypt cutover;
  nothing reads it yet.
- **D3 — unfilterable outcome channel. MEASURED 2026-08-03, decided.** Probe:
  a stream whose `/Filter` is the integer `3`, against qpdf 11.9.0.

  ```
  qpdf --show-object=4 --filtered-stream-data d3-filter-is-integer.pdf
    WARNING: (offset 250): stream filter type is not name or array
    WARNING: stream object 4 0: unable to filter stream data
    qpdf: unable to get object 4,0
    exit=2
  qpdf --show-object=4 --raw-stream-data d3-filter-is-integer.pdf
    exit=0
  ```

  So qpdf emits the warning **and then fails** on the `getStreamData` path.
  `Err` is confirmed as the right outcome for the native reader, and flpdf's
  message text already matches. The remaining divergence is only the
  *channel*: flpdf raises the text as an error and emits no accompanying
  warning. Do not "fix" that here — the native reader keeps flpdf's existing
  `Err`, and the channel gap is recorded in the beads issue.

- **D4 — null-valued `/DecodeParms` keys. MEASURED 2026-08-03, NOT fixed here
  (`flpdf-h8mv`).** `QPDF_Dictionary::getKeys` (`QPDF_Dictionary.cc:118-127`)
  skips every entry whose value `isNull()`, and
  `SF_FlateLzwDecode::setDecodeParms` iterates exactly that set
  (`SF_FlateLzwDecode.cc:29-31`). flpdf iterates unfiltered, so a null value
  arrives as `ParamValue::Other` and is rejected. Probe:

  ```
  /DecodeParms << /Predictor null >>  → qpdf exit=0 (filterable)
  /DecodeParms << /Predictor 5 >>     → qpdf exit=2 (control: really rejects)
  ```

  So qpdf tolerates it and flpdf errors. Pre-existing — the pre-Task-1 code
  reached the same rejection through `clamped_int_param`'s `None`. Fixing it
  changes public `decode_stream_data` behavior and must land in **both**
  readers at once (qpdf's `isNull()` dereferences, entangling it with D1), so
  it is tracked separately as `flpdf-h8mv`.

  **Both readers must keep flpdf's current behavior here**, so Task 7's
  equivalence gate still holds. Include the row in the corpus anyway, with a
  comment pointing at `flpdf-h8mv` — measuring the shape is what acceptance
  criterion 3 asks for; two readers agreeing is not evidence either is right.

- **D1 — indirect `/DecodeParms` values. MEASURED 2026-08-03, confirmed.**
  Probe: `/DecodeParms << /Predictor 12 /Columns 5 0 R >>` with object 5 = `4`,
  over two PNG `None`-filtered rows, compared against a direct `/Columns 4`.
  Both produced identical filtered bytes `[1,2,3,4,5,6,7,8]`, exit 0. A
  non-dereferencing reader would have fallen back to `columns = 1` and emitted
  5 bytes, so this discriminates. **qpdf dereferences**, which is why the
  native reader must use `try_*` and why the legacy `as_*` reader is the one
  that diverges here.

---

## Task 1: Make `FilterSpec` shape-neutral

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs:17-93` (`FilterSpec`,
  `decode_filter_specs`)
- Modify: `crates/flpdf/src/filters.rs:356`, `:515-561`, `:669-676`, `:700`
- Test: `crates/flpdf/src/stream_filter.rs` (in-file `#[cfg(test)] mod tests`)

**Step 1: Write the failing test**

Add to `stream_filter.rs` tests — a legacy-shape reader must produce a neutral
spec that still distinguishes absent/null params from a present non-dictionary:

```rust
#[test]
fn object_shape_reader_distinguishes_absent_null_and_present_non_dictionary() {
    let name = Object::Name(b"FlateDecode".to_vec());

    let absent = decode_filter_specs_from_object(Some(&name), None).unwrap();
    assert!(matches!(absent[0].decode_params, DecodeParams::Absent));

    let null = decode_filter_specs_from_object(Some(&name), Some(&Object::Null)).unwrap();
    assert!(matches!(null[0].decode_params, DecodeParams::Absent));

    let scalar = Object::Integer(1);
    let present = decode_filter_specs_from_object(Some(&name), Some(&scalar)).unwrap();
    // qpdf's SF_FlateLzwDecode treats a non-dictionary as an empty dictionary,
    // but the default StreamFilter::set_decode_params rejects any non-null.
    assert!(matches!(present[0].decode_params, DecodeParams::Present(_)));
    assert_eq!(present[0].decode_params.entries(), &[]);
}
```

**Step 2: Run it to verify it fails**

Run: `cargo test -p flpdf --lib stream_filter -- --nocapture`
Expected: FAIL — `decode_filter_specs_from_object` / `DecodeParams` not found.

**Step 3: Implement the neutral type and rename the reader**

```rust
/// Bounded `/DecodeParms` view: everything `StreamFilter::set_decode_params`
/// needs, with no `Object` or `ObjectHandle` left in it.
///
/// `Absent` covers both a missing key and an explicit null, matching
/// `QPDF_Stream::filterable`'s treatment of a null `/DecodeParms`.
/// `Present` carries the dictionary's entries in iteration order; a present
/// non-dictionary yields `Present` with no entries, mirroring
/// `SF_FlateLzwDecode::setDecodeParms`, which warns and treats a
/// non-dictionary as an empty dictionary while remaining filterable.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DecodeParams {
    Absent,
    Present(Vec<(Vec<u8>, ParamValue)>),
}

/// A `/DecodeParms` value reduced to the bounded scalars any filter reads.
///
/// `Int` carries `getIntValueAsInt`'s saturating clamp. `Name` exists for
/// `Crypt`'s `/Name`, which selects the crypt filter — carrying it now keeps
/// Phase 3's AES/Crypt cutover from having to widen this shared type.
/// `Other` is every remaining shape, which every current filter rejects the
/// same way `clamped_int_param` rejected a non-integer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ParamValue {
    Int(i32),
    Name(Vec<u8>),
    Other,
}

impl DecodeParams {
    pub(crate) fn is_absent(&self) -> bool { matches!(self, Self::Absent) }

    pub(crate) fn entries(&self) -> &[(Vec<u8>, ParamValue)] {
        match self {
            Self::Absent => &[],
            Self::Present(entries) => entries,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FilterSpec {
    pub(crate) name: Vec<u8>,
    pub(crate) decode_params: DecodeParams,
}
```

`decode_filter_specs` becomes `decode_filter_specs_from_object`, keeping the
exact branch order of `stream_filter.rs:46-92`, and converting each resolved
params object through a new private helper:

```rust
fn decode_params_from_object(params: Option<&Object>) -> DecodeParams {
    match params {
        None | Some(Object::Null) => DecodeParams::Absent,
        Some(object) => DecodeParams::Present(match object.as_dict() {
            Some(dict) => dict
                .iter()
                .map(|(key, value)| (key.to_vec(), param_value_from_object(value)))
                .collect(),
            None => Vec::new(),
        }),
    }
}

fn param_value_from_object(value: &Object) -> ParamValue {
    match clamped_int_param(value) {
        Some(int) => ParamValue::Int(int),
        None => match value.as_name() {
            Some(name) => ParamValue::Name(name.to_vec()),
            None => ParamValue::Other,
        },
    }
}
```

`FilterSpec` loses its lifetime, so drop `<'a>` from `PreparedDecodeFilter`,
`prepare_decode_filters`, and `decode_codec_prefix` in `filters.rs`.

**Step 4: Run the tests**

Run: `cargo test -p flpdf --lib`
Expected: PASS, including every pre-existing `filters` test unchanged.

**Step 5: Prove the public entry points did not move**

Run: `cargo test -p flpdf --test stream_decode_recovery_public_api`
Expected: PASS with zero edits to that file.

**Step 6: Commit**

```bash
git add crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs
git commit -m "refactor: give FilterSpec a shape-neutral DecodeParams"
```

---

## Task 2: Move `StreamFilter::set_decode_params` onto `DecodeParams`

> **Blocked by `flpdf-4rfl`.** Task 1's code review found that
> `filters.rs:675` wraps the side-effecting `adapter.set_decode_params(...)`
> in `debug_assert!`, which `cfg!(debug_assertions)` compiles out — so
> release builds decode the pending-boundary prefix with default predictor
> geometry and `decode_stream_data` returns a *different error* than in
> debug. Reproduced at `cf6b6885` and `aebb9446`:
> `cargo test --release -p flpdf --lib filters::` → 99 passed, 1 failed
> (`recovering_pending_error_precedes_equal_offset_final_finish_warning`),
> while the debug build passes. Task 2 rewrites that exact line onto the new
> signature, so fix `flpdf-4rfl` first or the swallowed side effect is
> carried into the new API.

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs:283-286` (trait default),
  `:377-434` (`FlateLzwStreamFilter`), and `png_encode_geometry`
  (`:761-773`) — the encode path calls `set_decode_params` too, so **Task 2
  does not compile until it is migrated as well**. Task 1's review surfaced
  this; the original file list missed it.
- Modify: `crates/flpdf/src/filters.rs:539`, `:676`, and `apply_encode_params`

**Step 1: Write the failing test**

```rust
#[test]
fn flate_filter_reads_predictor_geometry_from_neutral_params() {
    let mut filter = FlateLzwStreamFilter::new(false);
    let params = DecodeParams::Present(vec![
        (b"Predictor".to_vec(), ParamValue::Int(12)),
        (b"Columns".to_vec(), ParamValue::Int(4)),
    ]);
    assert!(filter.set_decode_params(&params));
    assert_eq!(filter.predictor, 12);
    assert_eq!(filter.columns, 4);
}

#[test]
fn non_null_params_still_make_a_parameterless_filter_unfilterable() {
    let mut filter = Ascii85StreamFilter;
    assert!(filter.set_decode_params(&DecodeParams::Absent));
    assert!(!filter.set_decode_params(&DecodeParams::Present(Vec::new())));
}

#[test]
fn a_fresh_flate_filter_accepts_both_present_shapes_the_neutral_form_merges() {
    // The neutral form collapses "present non-dictionary" and "present empty
    // dictionary" into `Present(vec![])`, removing flpdf's own early
    // `return true` for a non-dictionary (`stream_filter.rs:487-490`). That
    // shortcut was never qpdf's: `SF_FlateLzwDecode::setDecodeParms`
    // (`libqpdf/SF_FlateLzwDecode.cc:21-73`) early-returns only for
    // `isNull()` (`:24-26`); a present non-dictionary reaches `getKeys()`,
    // which warns `typeWarning("dictionary", "treating as empty")`
    // (`libqpdf/QPDFObjectHandle.cc:998-1009`), yields an empty set, and
    // falls through to the trailing `(predictor > 1) && (columns == 0)`
    // check at `:68-70`. So this merge is a CONVERGENCE toward qpdf, not a
    // tolerated loss.
    //
    // Both shapes still answer `true` because every caller applies params to
    // a freshly constructed adapter (defaults `predictor = 1, columns = 1`),
    // making that trailing check false either way.
    assert!(FlateLzwStreamFilter::new(false).set_decode_params(&DecodeParams::Present(Vec::new())));
    assert!(FlateLzwStreamFilter::new(true).set_decode_params(&DecodeParams::Present(Vec::new())));
}
```

> **Do not claim this test guards against adapter reuse.** An earlier draft
> ended the comment with "this assertion is what fails if an adapter is ever
> reused across specs". It does not: the test constructs its own fresh
> `FlateLzwStreamFilter`, so it stays green no matter what production does.
> Task 2's code review caught this by mutation testing. The property is
> pinned instead by Task 3's `a_dirtied_adapter_shows_why_absent_short_circuits_and_present_does_not`.

**Step 2: Run it to verify it fails**

Run: `cargo test -p flpdf --lib stream_filter`
Expected: FAIL — `set_decode_params` still takes `Option<&Object>`.

**Step 3: Change the signature**

```rust
fn set_decode_params(&mut self, decode_params: &DecodeParams) -> bool {
    decode_params.is_absent()
}
```

`FlateLzwStreamFilter::set_decode_params` keeps its exact key handling and the
trailing `(self.predictor > 1) && (self.columns == 0)` check, but iterates
`decode_params.entries()` and matches on `ParamValue::Int(v)` where it called
`clamped_int_param`; `ParamValue::Name(_) | ParamValue::Other` takes the arm
that `clamped_int_param`'s `None` took (`filterable = false`).
`clamped_int_param` is now called only from Task 1's
`param_value_from_object`.

**Step 4: Run the tests**

Run: `cargo test -p flpdf --lib`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/stream_filter.rs crates/flpdf/src/filters.rs
git commit -m "refactor: take DecodeParams in StreamFilter::set_decode_params"
```

---

## Task 3: Move the Crypt stage off `&Object`

**Files:**
- Modify: `crates/flpdf/src/filters.rs:342-352` (`F` bound),
  `:368-369`, `:505-513` (`PreparedStage::Crypt`), `:526-533`

**Step 1: Write the failing test**

```rust
#[test]
fn crypt_stage_receives_neutral_decode_params() {
    let mut dict = Dictionary::new();
    dict.insert("Filter", Object::Name(b"Crypt".to_vec()));
    let mut seen_absent = false;
    let result = decode_stream_data_with_filters_and_crypt(
        dict.get("Filter"),
        dict.get("DecodeParms"),
        b"payload",
        DecodeLimits::default(),
        DataEventMode::Suppress, // the enum has only Record and Suppress
        &mut |params: &DecodeParams, data: &[u8]| {
            seen_absent = params.is_absent();
            Ok(data.to_vec())
        },
    );
    assert!(result.is_ok());
    assert!(seen_absent);
}
```

> **`DecodeParams` no longer derives `Clone`.** Task 1's cleanup made
> `PreparedStage::Crypt` a unit variant reading `&stage.spec.decode_params`,
> which removed the last `clone()` caller, so the derive was dropped. The
> snippet above therefore inspects the borrow instead of cloning it; re-add
> the derive only if a real caller needs it. `ParamValue` still derives
> `Clone`.
>
> Task 1 also already made the Crypt arm a unit variant, so this task's only
> remaining work here is the closure bound and `decrypt_crypt(&stage.spec.decode_params, ...)`.

**Step 2: Run it to verify it fails**

Run: `cargo test -p flpdf --lib filters`
Expected: FAIL — closure bound is `FnMut(Option<&Object>, &[u8])`.

**Step 3: Change `PreparedStage::Crypt` to hold `DecodeParams` and the bound to
`F: FnMut(&DecodeParams, &[u8]) -> Result<Vec<u8>>`.** The default provider at
`filters.rs:334-338` keeps returning
`Error::Unsupported("unsupported stream filter: Crypt")` verbatim.

**Step 3b: Fold in Task 2's code-review follow-ups**

Task 2's review approved but left four items for this task, which edits the
same file. The first is mandatory before Task 5.

- **Split `clamped_int_param` (MANDATORY before Task 5).** It currently fuses
  shape-specific integer extraction (`Object::as_integer`) with the
  shape-independent saturation that *is* the `getIntValueAsInt` parity
  (`QPDFObjectHandle.cc:526-543`). Task 5's handle reader enters from
  `try_as_integer`, so leaving it fused makes that reader duplicate the
  parity-critical clamp — the exact duplication this plan exists to prevent.
  Extract `fn clamp_to_i32(value: i64) -> i32` and have both shape readers
  call it.
- **Pin the `is_absent()` early return.** The review deleted it and the whole
  suite stayed green (3116 passed), because every caller applies params to a
  freshly constructed adapter. Add the test below, which the reviewer verified
  goes red when the early return is removed:

```rust
#[test]
fn a_dirtied_adapter_shows_why_absent_short_circuits_and_present_does_not() {
    let mut filter = FlateLzwStreamFilter::new(false);
    // Assignment precedes the range check, so predictor=12, columns=0 stick.
    assert!(!filter.set_decode_params(&neutral_params(&[
        ("Predictor", ParamValue::Int(12)),
        ("Columns", ParamValue::Int(0)),
    ])));
    assert_eq!((filter.predictor, filter.columns), (12, 0));
    // SF_FlateLzwDecode.cc:24-26 returns true for isNull() regardless of
    // prior state. Removing the early return fails only this line.
    assert!(filter.set_decode_params(&DecodeParams::Absent));
    // Present-but-empty does not short-circuit: it reaches :68-70 still
    // carrying the old geometry.
    assert!(!filter.set_decode_params(&DecodeParams::Present(Vec::new())));
}
```

- **State the `ParamValue` invariant.** Its doc explains `Other` by reference
  to what `clamped_int_param` *used to* reject, which no filter calls any
  more. Replace that with the invariant a reader can check against qpdf:
  `ParamValue::Int` appears if and only if `QPDFObjectHandle::isInteger()`
  would be true; `Name` and `Other` are qpdf's `else` branch.
- **Rename `flate_lzw_filter_retains_only_the_qpdf_parameter_set`.** The
  property it was named for (not retaining a reference to the caller's
  object) became structurally impossible when `FilterSpec` lost its lifetime.
  Rename to what it now pins, or merge it into
  `flate_factory_treats_non_dictionary_decode_params_as_empty_like_qpdf`.

Watch for `-D warnings`: removing `decode_params_to_object` leaves
`stream_filter.rs:11`'s `Dictionary` import with no production user (the test
module has its own `use`), so clippy will fail on the unused import.

**Step 4: Run the tests**

Run: `cargo test -p flpdf --lib` — Expected: PASS.

**Step 5: Confirm no `Object` remains below the seam**

Run:
```bash
grep -n "Object" crates/flpdf/src/stream_filter.rs
```
Expected: matches only inside `decode_filter_specs_from_object` /
`decode_params_from_object` / `param_value_from_object` / `clamped_int_param`
and their tests.

**Step 6: Run the qpdf codec differential — the real gate for Tasks 1-3**

Tasks 1-3 are a behavior-preserving refactor of the parity-critical layer, and
until Task 7 exists the only thing checking them is the unit suite. Run the
absolute oracle now, with three commits of surface area rather than nine:

Run: `scripts/qpdf-stream-codecs-diff.sh`
Expected: PASS. If it fails, the refactor drifted warning order or predictor
geometry — bisect within Tasks 1-3 before going further.

**Step 7: Commit**

```bash
git add crates/flpdf/src/filters.rs crates/flpdf/src/stream_filter.rs
git commit -m "refactor: pass DecodeParams to the Crypt stage provider"
```

---

## Task 4: Add the `try_as_*` accessors the native reader needs

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs` (beside `try_as_dictionary`,
  `:541-571`)

**Step 1: Write the failing test**

```rust
#[test]
fn try_as_name_resolves_an_indirect_name_through_its_document() {
    let (handle, _pdf) = resolver_bearing_handle_for(ObjectValue::Name(b"FlateDecode".to_vec()));
    assert!(!handle.is_resolved());
    assert_eq!(handle.try_as_name().unwrap(), Some(b"FlateDecode".to_vec()));
    assert!(handle.is_resolved());
}

#[test]
fn try_as_integer_reports_a_dropped_document_rather_than_none() {
    let handle = dropped_document_handle();
    assert!(handle.try_as_integer().is_err());
}
```

Reuse whatever resolver-bearing test helper `object_handle.rs`'s existing
`try_get_key_resolves_the_same_indirect_slot_once` (`:1916`) and
`try_dereference_reports_a_dropped_document_without_reconnecting` (`:1931`)
already use; do not invent a second harness.

**Step 2: Run it to verify it fails**

Run: `cargo test -p flpdf --lib object_handle`
Expected: FAIL — `try_as_name` not found.

**Step 3: Implement, mirroring the existing `try_*` bodies exactly**

```rust
/// qpdf-compatible name inspection with lazy dereference.
#[allow(dead_code)] // promoted with complete resolver wiring in flpdf-25kg.3.5
pub(crate) fn try_as_name(&self) -> Result<Option<Vec<u8>>> {
    self.try_dereference()?;
    Ok(self.as_name())
}
```

Add the same three-line shape for `try_as_array` and `try_as_integer`. Each
carries the same `#[allow(dead_code)]` comment the `.3.3` accessors carry.

**Exactly three accessors, no more.** `try_as_dictionary` and `try_get_key`
already exist from `.3.3`, and Task 6's entry point takes a *stream dictionary
handle plus raw bytes* — matching AC2's wording and `.3.5`, which already holds
the ObjStm bytes from `readObjectAtOffset`. So `try_as_stream_dict` and
`try_as_stream_data` have no caller in this plan and must not be added:
`#[allow(dead_code)]` covers the decode entry points AC4 deliberately leaves
callerless, not speculative API.

**Step 4: Run the tests**

Run: `cargo test -p flpdf --lib object_handle`
Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat: add lazily-dereferencing ObjectHandle value accessors"
```

---

## Task 5: Native shape reader `decode_filter_specs_from_handle`

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs`
- Test: `crates/flpdf/src/stream_filter.rs` tests

**Step 0: DONE — D1 and D3 are measured.** See "Decisions recorded up front"
above for the probe commands, outputs, and what they settled. The probe
generator is `scratchpad/make_probe.py`; re-run it if a corpus row needs a
fresh observation.

For the record, since it is easy to get wrong: **do not probe via
`--json=2 --json-stream-data=inline`.** `QPDF_Stream::writeStreamJSON`
(`QPDF_Stream.cc:252-262`) retries with `decode_level = qpdf_dl_none` when the
first `pipeStreamData` attempt fails and then emits raw bytes, so a broken
`/Filter` comes back looking *tolerated*. The `getStreamData` path
(`:345-358`) is the one that fails, and `--show-object=N
--filtered-stream-data` is what exercises it.

**Step 0b: D1's acceptance test lives here, not in Task 7**

Dereferencing is the entire reason the native reader exists, so it needs a
focused test next to the reader — and Task 7's equivalence gate structurally
cannot provide one (see the note in Task 7).

Write a test that gives the native reader an **indirect** `/Filter` and an
**indirect** `/DecodeParms` value through the `RecordingResolver` harness
Task 4 extended (`object_handle.rs:1897-1946`), and asserts the reader resolves
them the way `QPDF_Stream::filterable`'s `getKey`/`getArrayItem` do
(`QPDF_Stream.cc:386`, `:403`, `:440`). Cover at least: an indirect name
`/Filter`; an indirect array item; an indirect integer `/DecodeParms` value;
and a handle whose document was dropped, which must surface
`try_dereference`'s existing `Error::Internal` rather than silently reading as
absent.

This is what makes decision D1 real. Without it, D1 ships untested.

**Step 1: Write the failing test**

```rust
#[test]
fn handle_reader_matches_object_reader_for_every_filter_shape() {
    for (filter, parms) in shape_corpus() {
        let from_object = decode_filter_specs_from_object(filter.as_object(), parms.as_object());
        let from_handle = decode_filter_specs_from_handle(&filter.as_handle(), &parms.as_handle());
        assert_eq!(from_object, from_handle, "shape {filter:?} / {parms:?} diverged");
    }
}
```

> `FilterSpec` derives `PartialEq` (added in Task 2 and, as Task 3's review
> noted, still without a user), so compare the values directly. An earlier
> draft compared `format!("{:?}")` strings; that is strictly weaker and its
> failure output is unreadable.

`shape_corpus()` must cover, at minimum: absent `/Filter`; `Null`; a name; an
abbreviation (`Fl`, `AHx`); an array of names; an array containing a non-name;
a non-name non-array scalar; an empty `/DecodeParms` array; an aligned array; a
misaligned array; a scalar `/DecodeParms` replicated across two filters; a
`/DecodeParms` dictionary with `Predictor`/`Columns`/`Colors`/`BitsPerComponent`/
`EarlyChange`; a non-integer value for each of those; a `Crypt` filter; and an
unknown filter name.

Add one case the earlier draft missed, since Step 3 moves the chain-count
pre-check into the readers: **an over-long filter array whose last element is
also a non-name** (e.g. 17 entries with a trailing integer). That is the
`max_filter_chain`-before-malformed-item precedence rule, pinned for the
legacy path by `decode_rejects_overlong_filter_chain_before_malformed_item`
(`filters.rs:2678`) and otherwise unpinned for the native one.

**Step 2: Run it to verify it fails**

Run: `cargo test -p flpdf --lib stream_filter`
Expected: FAIL — `decode_filter_specs_from_handle` not found.

**Step 3: Implement the native reader**

Same branch order as `decode_filter_specs_from_object`, reached through
`try_as_name` / `try_as_array` / `try_as_dictionary` / `try_as_integer` so each
child dereferences the way `QPDF_Stream::filterable`'s `getKey`/`getArrayItem`
do. Deliberately **not** shared with the object reader: the two differ in how a
value is inspected, and merging them behind a trait would reintroduce a wrapper
the DESIGN forbids. The shared part is everything downstream of `FilterSpec`.

**Step 4: Run the tests**

Run: `cargo test -p flpdf --lib stream_filter` — Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/stream_filter.rs
git commit -m "feat: read filter specs from an ObjectHandle stream dictionary"
```

---

## Task 6: Native decode entry point

**Files:**
- Modify: `crates/flpdf/src/filters.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn native_entry_point_decodes_a_flate_stream_from_a_handle() {
    let payload = b"canonical resolver payload";
    let encoded = crate::stream_filter::encode_flate(payload).unwrap();
    let dict = ObjectHandle::dictionary(vec![
        (b"Filter".to_vec(), ObjectHandle::name(b"FlateDecode".to_vec())),
        (b"Length".to_vec(), ObjectHandle::integer(encoded.len() as i64)),
    ]);
    let decoded = decode_stream_data_from_handle(&dict, &encoded, DecodeLimits::default()).unwrap();
    assert_eq!(decoded, payload);
}
```

**Step 2: Run it to verify it fails**

Run: `cargo test -p flpdf --lib filters` — Expected: FAIL, function not found.

**Step 3: Implement**

Split the existing engine so the shared half takes specs, not objects:

```rust
fn decode_prepared_specs(
    specs: Vec<FilterSpec>,
    stream_data: &[u8],
    limits: DecodeLimits,
    data_events: DataEventMode,
    decrypt_crypt: &mut dyn FnMut(&DecodeParams, &[u8]) -> Result<Vec<u8>>,
) -> Result<StreamDecodeOutcome>
```

`decode_stream_data_with_filters_and_crypt` keeps its signature and becomes
`decode_filter_specs_from_object(...)` + `decode_prepared_specs(...)`. The
native entry point is `decode_filter_specs_from_handle(...)` +
`decode_prepared_specs(...)`, so both share one staging/warning-order engine.

**Expose the native path at BOTH levels.** A strict
`decode_stream_data_from_handle(...) -> Result<Vec<u8>>` alone cannot carry a
warning sequence, and Task 7 has to compare warnings — messages, codes, and
order — or acceptance criterion 3's "warning order against qpdf" goes
untested and that gate silently degrades into a bytes-only comparison. Since
`decode_prepared_specs` already returns `StreamDecodeOutcome`, also add an
outcome-level `pub(crate)` variant mirroring
`decode_stream_data_recovering_with_limits` /
`decode_stream_data_with_limits_and_warnings`, and let the strict `Vec<u8>`
form sit on top of it through `replay_strict_decode_event`, exactly as the
legacy strict path does. `flpdf-25kg.3.5` only needs the strict
`getStreamData`-shaped one, so the outcome-level variant stays `pub(crate)`.

**Do not mirror the raw-array chain-count pre-check — move it into the shape
readers.** `filters.rs:353-355` currently runs
`validate_filter_chain_count(filters.len(), ...)` on the raw `Object::Array`
*before* building specs, so an over-long chain is reported ahead of a
non-name item or a `/DecodeParms` length mismatch. That precedence rule is
pinned by `decode_rejects_overlong_filter_chain_before_malformed_item`
(`filters.rs:2678`); Task 3's review confirmed it by deleting the block and
watching only that test go red.

It is also the **last `Object`-shaped assumption sitting above `FilterSpec`**.
An earlier draft of this task told the native reader to mirror it from
`try_as_array` — which would have created exactly the second copy this plan
exists to prevent, this time for error precedence rather than warning order,
and Task 7's corpus does not cover it.

Push it down instead: give each shape reader the limit and let it perform
both counts, e.g.
`decode_filter_specs_from_object(filter, params, max_filter_chain)` and
`decode_filter_specs_from_handle(filter, params, max_filter_chain)`. After
that, nothing above `FilterSpec` is shape-dependent and
`decode_prepared_specs` needs no pre-check of its own.

**Step 4: Run the tests**

Run: `cargo test -p flpdf --lib` — Expected: PASS.

**Step 5: Commit**

```bash
git add crates/flpdf/src/filters.rs
git commit -m "feat: decode stream data from an ObjectHandle dictionary"
```

---

## Task 7: Legacy-vs-native equivalence corpus (AC3 + AC4 in one gate)

**Files:**
- Modify: `crates/flpdf/src/filters.rs` — add the equivalence corpus as an
  **in-crate** `#[cfg(test)]` module, not an integration test.

> **Why in-crate, corrected after Task 5's review.** `lib.rs:167` declares
> `pub(crate) mod stream_filter;`, so a file under `crates/flpdf/tests/`
> compiles as a separate crate and cannot reach `decode_filter_specs_from_object`,
> `decode_filter_specs_from_handle`, `FilterSpec`, `DecodeParams`, or Task 5's
> `shape_corpus`/`handle_from_object` helpers. Task 6 also keeps its
> outcome-level entry point `pub(crate)`. An earlier draft asked for an
> integration test, which is incompatible with both.
>
> Do **not** resolve this by widening visibility. `flpdf-25kg.3.3` deliberately
> kept the `try_*` accessors crate-private (PR #620 review) until `.3.5` wires
> the resolver, and `.3.5` is itself in-crate, so nothing here needs a public
> surface. The `qtest-driver` feature gate used for `ref_chain`/`tokenizer`
> (`lib.rs:151-155`, `:170-174`) exists for an external driver — this is not
> that case.

> **This gate covers a DIRECT-ONLY corpus, and that limit is load-bearing.**
> Decision D1 makes the two readers *deliberately* disagree on an indirect
> child: the legacy `as_*` reader turns `Object::Reference` into
> `ParamValue::Other` (so `filterable = false`), while the native `try_*`
> reader dereferences and sees the real value — which the 2026-08-03 probe
> confirmed is what qpdf does. Asserting strict equivalence over an indirect
> case would therefore assert the wrong thing.
>
> The trap is the *other* direction: if `shape_corpus()` builds handles with
> `ObjectHandle::dictionary(...)` and direct children — the obvious way to
> write it — then `try_dereference` short-circuits on `Repr::Direct`, no
> indirect case is ever reached, this gate passes, and D1 ships untested.
> That is why D1's acceptance test is Task 5 Step 0b, next to the reader it
> validates. State the exclusion in this file's header comment so a later
> reader does not "fix" the gap by widening the corpus here.

**Step 1: Write the test**

For every case in the Task 5 corpus, plus real encoded payloads (flate,
flate+PNG predictor, LZW with and without `EarlyChange`, ASCII85, ASCIIHex,
RunLength, a chain of two), assert that the legacy `&Dictionary` entry point and
the native `ObjectHandle` entry point return the **same** `Ok`/`Err` and the
**same warning sequence** — messages, codes, and order. Include the
`max_output` limit case and the malformed/truncated-flate cases that
`stream_decode_recovery_public_api.rs` already pins.

The point of this module is that it fails loudly if the native reader diverges
from the legacy one.

> **Corrected after implementation: this is a RELATIVE gate, and one assertion
> does NOT cover both AC3 and AC4.** An earlier draft claimed it did. Mutation
> testing disproved that: reversing `outcome.events` inside the *shared*
> `decode_prepared_specs` leaves the equivalence assertion **green** — both
> entry points drift together — while ~20 absolute tests go red. A relative
> gate can only see the two paths disagreeing, never the engine beneath them
> moving.
>
> So the module needs an **absolute companion** that pins real warning text and
> codes, not just agreement (`the_corpus_reaches_multi_event_warning_sequences`).
> Note that even that test was initially too weak — comparing only event
> variant names let a `code - 1` mutation through — so it must assert the exact
> message and code. `scripts/qpdf-stream-codecs-diff.sh` remains the absolute
> qpdf truth for the codec layer; this module is the relative legacy-vs-native
> one. Both are required.

> **Module privacy applies inside a crate too.** `filters::tests` cannot see
> `stream_filter::tests::shape_corpus` merely by being in the same crate, so
> reusing Task 5's corpus helpers needs `pub(crate)` on that `#[cfg(test)] mod
> tests` and on the two helpers — the same arrangement Task 4 established for
> `object_handle::identity_tests`. That is a test-only change; **no production
> visibility may widen.**

**Carry the `max_filter_chain` dimension up from Task 5.** Task 5's *unit*
corpus already sweeps `[None, Some(16), Some(0)]`. `DecodeLimits` is `pub` with
a `pub max_filter_chain` field and `decode_stream_data_with_limits` is `pub`,
so `Some(0)` is genuinely reachable by an embedder — Task 5's review proved it
by compiling a probe that got
`filter chain length 1 exceeds maximum of 0` out of the public API. If this
entry-point corpus sweeps only the default limit, that live public behavior
ships uncompared between the two entry points.

**Step 2: Run it**

Run: `cargo test -p flpdf --lib equivalence`
Expected: PASS.

**Step 3: Commit**

```bash
git add crates/flpdf/src/filters.rs
git commit -m "test: pin legacy and ObjectHandle decode equivalence"
```

---

## Task 8: qpdf differential + docs

**Files:**
- Modify: `crates/flpdf/src/stream_filter.rs` module doc (D4 correspondence line)
- Modify: `docs/qpdf-correspondence.md` row for `QPDFStreamFilter.cc`
- Possibly modify: `scripts/qpdf-stream-codecs-diff.sh`

**Step 1: Re-run the codec differential over the finished branch**

Run: `scripts/qpdf-stream-codecs-diff.sh`
Expected: PASS. Task 3 Step 6 already ran this over the refactor alone; this
re-run covers the native reader and entry point too. It is the absolute qpdf
truth for the codec layer, where Task 7 is the relative legacy-vs-native gate —
both are required.

**Step 2: Update the module doc**

The classifier requires the classification to be **a single line ending in a
period** (this broke Quality CI on PR #620). After editing, run:

```bash
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
scripts/qpdf-module-docs.py
```
Expected: PASS.

**Step 3: Commit**

```bash
git add crates/flpdf/src/stream_filter.rs docs/qpdf-correspondence.md
git commit -m "docs: record the ObjectHandle-native decode boundary"
```

---

## Task 9: Verification and acceptance evidence

**REQUIRED SUB-SKILL:** superpowers:verification-before-completion

Run every command and paste real output into the beads issue notes. Do not
claim a gate passed without its output.

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/qpdf-stream-codecs-diff.sh
python3 -m unittest scripts/tests/test_qpdf_module_docs.py
```

Changed-line coverage (must be 100%, AC5) — note the feature flag, per the
`llvm-cov-no-qpdf-zlib-compat` memory:

```bash
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path lcov.info
scripts/patch-coverage.sh --base origin/main
```

Baseline for comparison, captured before any edit on this branch:
**147 suites, 5866 passed, 0 failed.**

Then record in `bd`:
- the D3 live-probe observation from Task 5 Step 0;
- the confirmation that the native functions have zero production callers
  (AC4) and which issue picks them up (`flpdf-25kg.3.5`);
- exact verification commands and their results.

Finally: `cargo fmt` before pushing (`flpdf-ci-quality-fmt-check` memory —
CI Quality is `cargo fmt --check`).
