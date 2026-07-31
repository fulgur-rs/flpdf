# ObjectHandle + Pdf Public Handle API Surface Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> to implement this plan task-by-task.

**Goal:** Round out `ObjectHandle`'s public accessor surface (and promote the
one still-`pub(crate)` state query it needs) so external consumers — starting
with `flpdf-qtest-tools::driver::Handle` in the very next stack layer — can be
built on it, without touching a single existing consumer call site in this
layer.

**Architecture:** All new surface lives in the existing
`crates/flpdf/src/object_handle.rs` file only. Two new `ObjectValue` variants
(`Operator`, `InlineImage`) close a real representational gap (content-stream
tokens have no `ObjectHandle` form today); six new typed accessors
(`as_boolean`, `as_real`, `as_name`, `as_string`, `as_operator`,
`as_inline_image`, `as_reference`) round out the existing `as_integer`/
`as_array`/`as_dictionary`/`as_stream_dict`/`as_stream_data`/`as_real_literal`
family; `is_resolved` is promoted from `pub(crate)` to `pub`; and
`type_code`/`type_name`/`unparse`/`unparse_resolved` are new qpdf-shaped
operations the design doc names explicitly. `unparse`/`unparse_resolved` are
implemented by delegating to the existing `pub(crate) materialize()` bridge
and `Object::write_pdf` — reusing already-tested byte-serialization logic
rather than duplicating it — which is an internal implementation detail, not
a new public dependency on `Object` (see Task 6's note on why this is safe to
replace later without an API break).

**Post-implementation update (2026-08-01):** the plan below was executed as
Tasks 1-6 (`object_handle.rs` only), but two Codex Review findings during PR
#603 required `crates/flpdf/src/reader.rs` changes too: a native-parsed
string was never decrypted before a new accessor could read it (violating a
hard precondition `flpdf-jjxb` had already recorded), and a stream
dictionary's own inline-nesting depth was double-counted. Neither touches an
existing *consumer's* call site — both are population/decrypt-correctness
fixes underneath the new accessors — and flpdf-egzr.3.2.1's actual AC7 always
permitted this ("no file outside `crates/flpdf/src/object_handle.rs` and
`crates/flpdf/src/reader.rs` changes behavior"); this doc's narrower
"`object_handle.rs` only" framing below was a self-imposed tightening that
didn't anticipate either finding. See the two `fix(reader):` commits on this
branch and flpdf-egzr.3.2.1's notes for the full detail.

**Tech Stack:** Rust 2021 workspace; pinned qpdf 11.9.0 source
(`include/qpdf/QPDFObjectHandle.hh`, `include/qpdf/Constants.h`,
`libqpdf/QPDFObjectHandle.cc`, `libqpdf/QPDF_Stream.cc`) as the behavioral
oracle for `type_code`/`type_name`/`unparse`/`unparseResolved`;
`flpdf-qtest-tools::driver::Handle` (`crates/flpdf-qtest-tools/src/driver/handle.rs`)
as the existing prototype for the same operations from a different angle;
existing `cargo test`, Clippy, `cargo llvm-cov`, `scripts/patch-coverage.sh`.

---

## Status and Prior Context

- Parent stack: `flpdf-egzr.3.2` ("ObjectHandle consumer cutover and legacy
  Object removal"), itself split into 8 sequential sub-issues
  (`flpdf-egzr.3.2.1` .. `.8`) because the full cutover touches thousands of
  call sites across dozens of files — too large for one PR or one
  `scripts/patch-coverage.sh` gate. This plan implements **only**
  `flpdf-egzr.3.2.1`, the first slice.
- Design (approved 2026-07-30, now on `main`):
  `docs/superpowers/specs/2026-07-30-xref-parsed-offset-object-handle-design.md`
  — read this file in full before starting; this plan does not repeat its
  general architecture citations, only the ones specific to the new surface
  added here.
- Prior layer `flpdf-egzr.3.1` ("ObjectHandle graph and reader cutover") is
  closed and merged to `main` (PR #599, merge commit `0138c3f7`, plus PR #600
  landing the design doc itself). `main` at `0138c3f7` is this plan's base —
  **not** any older commit or worktree.
- `bd update flpdf-egzr.3.2.1 --claim` has already been run.
- Worktree: `/home/ubuntu/flpdf/.worktrees/flpdf-egzr-3-2-1-objecthandle-api`
  on branch `feat/flpdf-egzr-3-2-1-objecthandle-api`, branched from `main` at
  `0138c3f7`.
- A clean baseline (`cargo build --workspace`, `cargo test -p flpdf --lib` —
  2918 passed, 0 failed, 9 ignored) has already been verified green in this
  session at this worktree's base commit. A fresh executor (new
  subagent/session) must redo this (Task 1) since it has no memory of that
  run.
- Research already done this session, so later tasks don't need to
  re-derive it:
  - `Pdf::get_object_handle`, `Pdf::resolve_object_handle`,
    `Pdf::get_all_object_handles`, and `Pdf::trailer_handle` (all in
    `crates/flpdf/src/reader.rs`) are **already `pub`**. The gap is entirely
    on `ObjectHandle` itself, not on `Pdf`. This plan touches **zero**
    `Pdf`-side code.
  - `ObjectHandle`'s already-`pub` methods (`is_direct`, `is_indirect`,
    `object_ref`, `integer`, `get_parsed_offset`, `null`, `boolean`, `real`,
    `name`, `string`, `array`, `dictionary`, `real_literal`,
    `as_real_literal`, `is_null`, `as_integer`, `as_array`, `as_dictionary`,
    `as_stream_dict`, `as_stream_data`) already cover scalars other than
    boolean/name/string/operator/inline-image, plus array/dictionary/stream
    traversal. This plan adds exactly the accessors listed in Task 3, no
    more — grounded in (a) what `Object`/`Dictionary` already expose as its
    own accessor family (`crates/flpdf/src/object.rs:252-414`) and (b) what
    `flpdf-qtest-tools::driver::Handle` already needs from a generic handle
    type (`is_indirect`, `is_null`, `as_bool`, `type_code`, `type_name`,
    `unparse`, `unparse_resolved` — `crates/flpdf-qtest-tools/src/driver/handle.rs:95-192`).
    `array_item_indirectness`/`dictionary_items` are **not** ported here:
    they are fully composable from `as_array`/`as_dictionary` (already
    public) plus per-child `is_indirect()` (already public) — no new
    `ObjectHandle` method earns its keep for those two.
  - `ObjectValue` (`crates/flpdf/src/object_handle.rs:82-122`) has no
    `Operator`/`InlineImage` variants, even though the legacy `Object` enum
    does (`crates/flpdf/src/object.rs:142-172`, used by content-stream
    tokenization). This is a real gap this plan closes (Task 2) — not
    speculative, since without it no future slice could ever represent a
    content-stream token as an `ObjectHandle`.
  - `ObjectValue::Reference(ObjectRef)` exists for the case where an
    indirect object's own resolved value is itself a bare reference
    (`Pdf::set_object`-driven redirect/collapse chains — see
    `object_handle.rs:111-121`'s own doc comment). `Pdf::resolve_object_handle`
    can leave a handle resolved to this variant (it lifts whatever the
    legacy cache holds, and the legacy cache can hold `Object::Reference`
    for exactly this reason), so it **is** externally observable post-resolution
    and needs an accessor (Task 3's `as_reference`) — this is not the same
    thing as an indirect *child* handle (which is already exposed via
    `is_indirect()`/`object_ref()` on the child handle itself, no new
    accessor needed there).

## Global Constraints

- **Zero edits outside this allowlist:** `crates/flpdf/src/object_handle.rs`
  for Tasks 1-6, for all production code. If a task appears to require
  touching any other file (including `crates/flpdf/src/lib.rs` —
  `ObjectValue` stays `pub(crate)` and is not re-exported; only
  `ObjectHandle` itself is, and that re-export already exists), stop and
  reconsider — this plan's whole point is a zero-consumer-diff slice.
  `crates/flpdf/src/reader.rs` is also in scope, but only for a
  population/decrypt-correctness fix underneath a new accessor added by
  this plan, never for touching an existing consumer's call site — see the
  "Post-implementation update" note above and flpdf-egzr.3.2.1's actual
  AC7, which names both files explicitly. New test code lives in the same
  file's existing `#[cfg(test)]` module tree (new `mod` blocks alongside
  `identity_tests`/`object_value_tests`/`parsed_offset_tests`/
  `resolution_state_tests`/`materialize_tests`), not a new top-level test
  file — this plan adds no integration-level behavior, only unit-testable
  accessors on an already-unit-tested type.
- Every new `pub fn` needs a one-line doc comment (imperative, English —
  `.claude/rules/pdf-rust-doc-review-patterns.md` §3, §5) and, for anything
  citing qpdf, a real citation (file + line in the pinned source under
  `/tmp/qpdf-11.9.0-source`, or whichever path `scripts/fetch-qpdf-source.sh`
  materializes it at for the executing session — re-run that script first if
  the path from this plan's research doesn't exist).
- No behavior change to any existing method. This task set is additive only.

## File Structure

All changes: `crates/flpdf/src/object_handle.rs` (single file, six ordered
edits below, one commit per task).

---

### Task 1: Confirm clean baseline

**Step 1: Build and test**

Run: `cargo build --workspace && cargo test -p flpdf --lib`
Expected: clean build; the full `flpdf` lib test suite passes (baseline in
this session: 2918 passed, 0 failed, 9 ignored — a fresh run may differ
slightly if `main` has moved, but must be all-green either way).

**Step 2: No commit** (verification only).

---

### Task 2: `ObjectValue::Operator`/`InlineImage` — close the content-stream-token representation gap

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod token_value_tests {
    use super::*;

    #[test]
    fn operator_handle_round_trips_its_bytes() {
        let handle = ObjectHandle::operator(b"q".to_vec());
        assert_eq!(handle.as_operator(), Some(b"q".to_vec()));
        assert!(handle.as_inline_image().is_none());
    }

    #[test]
    fn inline_image_handle_round_trips_its_bytes() {
        let handle = ObjectHandle::inline_image(b"\x00\x01raw".to_vec());
        assert_eq!(handle.as_inline_image(), Some(b"\x00\x01raw".to_vec()));
        assert!(handle.as_operator().is_none());
    }

    #[test]
    fn operator_and_inline_image_materialize_to_the_matching_object_variant() {
        assert_eq!(
            ObjectHandle::operator(b"Do".to_vec()).materialize(),
            Object::Operator(b"Do".to_vec())
        );
        assert_eq!(
            ObjectHandle::inline_image(b"data".to_vec()).materialize(),
            Object::InlineImage(b"data".to_vec())
        );
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::token_value_tests`
Expected: FAIL to compile — `ObjectHandle::operator`/`inline_image` and
`as_operator`/`as_inline_image` don't exist yet.

**Step 3: Write the minimal implementation**

Add two variants to `ObjectValue` (`object_handle.rs:82-122`), removing the
now-satisfied `#[allow(dead_code)]` markers that were placeholders for this
exact task on the *other* not-yet-landed accessors (leave those markers
alone — they belong to Task 3, not this one):

```rust
pub(crate) enum ObjectValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    RealLiteral { value: f64, literal: Vec<u8> },
    Name(Vec<u8>),
    String(Vec<u8>),
    /// A content-stream operator token (e.g. `q`, `Do`), mirroring
    /// [`crate::Object::Operator`]. Only meaningful inside a content stream
    /// (`include/qpdf/QPDFObjectHandle.hh:317-318`: "Operator and
    /// InlineImage are only allowed in content streams").
    Operator(Vec<u8>),
    /// Raw inline-image (`BI`...`ID`...`EI`) bytes, mirroring
    /// [`crate::Object::InlineImage`]. Same content-stream-only constraint
    /// as `Operator` above.
    InlineImage(Vec<u8>),
    Array(Vec<ObjectHandle>),
    Dictionary(std::collections::BTreeMap<Vec<u8>, ObjectHandle>),
    Stream { dict: ObjectHandle, data: Vec<u8> },
    Reference(ObjectRef),
}
```

Add constructors (near `ObjectHandle::string`, same style):

```rust
    /// Construct a direct content-stream operator token value.
    pub fn operator(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::Operator(value), NO_PARSED_OFFSET)
    }

    /// Construct a direct raw inline-image byte payload value.
    pub fn inline_image(value: Vec<u8>) -> Self {
        Self::new_direct(ObjectValue::InlineImage(value), NO_PARSED_OFFSET)
    }
```

Add accessors (near `as_stream_data`, same `with_value` pattern):

```rust
    /// The value as raw operator bytes if this handle's value — its own if
    /// direct, or its already-resolved value if indirect — is a
    /// content-stream operator token, or `None` otherwise. Never performs
    /// resolution itself.
    pub fn as_operator(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Operator(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The value as raw inline-image bytes if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is an
    /// inline-image payload, or `None` otherwise. Never performs resolution
    /// itself.
    pub fn as_inline_image(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::InlineImage(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }
```

Add the two new arms to `materialize_value` (`object_handle.rs:577-613`):

```rust
        ObjectValue::Operator(bytes) => Object::Operator(bytes.clone()),
        ObjectValue::InlineImage(bytes) => Object::InlineImage(bytes.clone()),
```

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib object_handle::`
Expected: all `object_handle` module tests pass, including the new
`token_value_tests` group.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add Operator/InlineImage value representation"
```

---

### Task 3: Round out typed scalar/reference accessors

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod rounded_accessor_tests {
    use super::*;

    #[test]
    fn boolean_handle_round_trips_its_value() {
        assert_eq!(ObjectHandle::boolean(true).as_boolean(), Some(true));
        assert_eq!(ObjectHandle::boolean(false).as_boolean(), Some(false));
        assert_eq!(ObjectHandle::integer(1).as_boolean(), None);
    }

    #[test]
    fn as_real_accepts_both_real_and_real_literal_like_object_does() {
        // Mirrors Object::as_real's own `Real(v) | RealLiteral { value: v, .. }`
        // arm (object.rs:348-353) — a real-literal value is still "a real"
        // for callers that don't care about the source spelling.
        assert_eq!(ObjectHandle::real(1.5).as_real(), Some(1.5));
        assert_eq!(
            ObjectHandle::real_literal(0.4, b".4".to_vec()).as_real(),
            Some(0.4)
        );
        assert_eq!(ObjectHandle::integer(1).as_real(), None);
    }

    #[test]
    fn name_and_string_handles_round_trip_their_bytes() {
        assert_eq!(
            ObjectHandle::name(b"Type".to_vec()).as_name(),
            Some(b"Type".to_vec())
        );
        assert_eq!(
            ObjectHandle::string(b"hi".to_vec()).as_string(),
            Some(b"hi".to_vec())
        );
        assert!(ObjectHandle::name(b"Type".to_vec()).as_string().is_none());
        assert!(ObjectHandle::string(b"hi".to_vec()).as_name().is_none());
    }

    #[test]
    fn as_reference_reads_a_resolved_indirect_redirect_but_not_a_plain_value() {
        // ObjectValue::Reference is what an indirect handle resolves to when
        // its own body is itself a bare reference (Pdf::set_object-driven
        // redirect/collapse chains — see ObjectValue::Reference's own doc).
        let redirect = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        redirect.set_resolved(ObjectValue::Reference(ObjectRef::new(9, 0)));
        assert_eq!(redirect.as_reference(), Some(ObjectRef::new(9, 0)));
        assert_eq!(ObjectHandle::integer(1).as_reference(), None);
    }

    #[test]
    fn rounded_accessors_return_none_for_an_indirect_handle_before_resolution() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        assert_eq!(handle.as_boolean(), None);
        assert_eq!(handle.as_real(), None);
        assert!(handle.as_name().is_none());
        assert!(handle.as_string().is_none());
        assert_eq!(handle.as_reference(), None);
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::rounded_accessor_tests`
Expected: FAIL to compile — none of `as_boolean`/`as_real`/`as_name`/
`as_string`/`as_reference` exist yet.

**Step 3: Write the minimal implementation**

Add accessors (near the existing `as_integer`/`as_real_literal`, same
`with_value` pattern; also delete the now-satisfied
`#[allow(dead_code)] // as_boolean/as_real/as_name/as_string accessor lands
in a later task` markers on the corresponding `ObjectValue` variants at
`object_handle.rs:84-99`):

```rust
    /// The value as `bool` if this handle's value — its own if direct, or
    /// its already-resolved value if indirect — is a boolean, or `None`
    /// otherwise. Never performs resolution itself.
    pub fn as_boolean(&self) -> Option<bool> {
        self.with_value(|value| match value {
            Some(ObjectValue::Boolean(b)) => Some(*b),
            _ => None,
        })
    }

    /// The value as `f64` if this handle's value — its own if direct, or
    /// its already-resolved value if indirect — is a real number (including
    /// one with a preserved non-canonical source literal), or `None`
    /// otherwise. Mirrors [`crate::Object::as_real`]'s own real-or-real-literal
    /// arm. Never performs resolution itself.
    pub fn as_real(&self) -> Option<f64> {
        self.with_value(|value| match value {
            Some(ObjectValue::Real(v) | ObjectValue::RealLiteral { value: v, .. }) => Some(*v),
            _ => None,
        })
    }

    /// The value as decoded PDF name bytes if this handle's value — its own
    /// if direct, or its already-resolved value if indirect — is a name, or
    /// `None` otherwise. Never performs resolution itself.
    pub fn as_name(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::Name(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The value as string bytes if this handle's value — its own if
    /// direct, or its already-resolved value if indirect — is a string, or
    /// `None` otherwise. Never performs resolution itself.
    pub fn as_string(&self) -> Option<Vec<u8>> {
        self.with_value(|value| match value {
            Some(ObjectValue::String(bytes)) => Some(bytes.clone()),
            _ => None,
        })
    }

    /// The target as an indirect-object reference if this handle's value —
    /// its own if direct, or its already-resolved value if indirect — is
    /// itself a bare reference (a `Pdf::set_object`-driven redirect; see
    /// [`ObjectValue::Reference`]'s own doc), or `None` otherwise. This is
    /// distinct from an indirect *child* handle, which is exposed via
    /// [`Self::is_indirect`]/[`Self::object_ref`] on the child handle itself
    /// rather than through this accessor. Never performs resolution itself.
    pub fn as_reference(&self) -> Option<ObjectRef> {
        self.with_value(|value| match value {
            Some(ObjectValue::Reference(object_ref)) => Some(*object_ref),
            _ => None,
        })
    }
```

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib object_handle::`
Expected: all pass, including `rounded_accessor_tests`.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): round out boolean/real/name/string/reference accessors"
```

---

### Task 4: Promote `is_resolved` to public

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`

**Step 1: Write the failing test**

```rust
#[cfg(test)]
mod is_resolved_visibility_tests {
    use super::*;

    #[test]
    fn is_resolved_is_usable_the_same_way_a_pub_fn_is() {
        // This test doesn't exercise new behavior (resolution_state_tests
        // already covers is_resolved's semantics exhaustively) — it exists
        // only to keep a compile-time witness that `is_resolved` stays
        // `pub`, the same way the rest of this module's public surface has
        // a direct caller in-tree. Real external verification happens in
        // Task 5 (zero-consumer-diff gate does not apply to this file
        // itself, so a positive compile check here is the useful signal).
        let handle = ObjectHandle::integer(1);
        let _: bool = ObjectHandle::is_resolved(&handle);
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::is_resolved_visibility_tests`
Expected: this actually still **passes to compile** today since the test is
in the same crate (`pub(crate)` is visible within the crate) — so this step
is a rare case where RED doesn't show up as a compile failure. Instead,
verify the *current* visibility directly:
Run: `grep -n 'fn is_resolved' crates/flpdf/src/object_handle.rs`
Expected (before this task): `pub(crate) fn is_resolved`.

**Step 3: Write the minimal implementation**

Change the signature at `object_handle.rs:324` from:

```rust
    pub(crate) fn is_resolved(&self) -> bool {
```

to:

```rust
    /// True if this handle's value is known without performing resolution:
    /// a direct handle always is; an indirect handle is once its state has
    /// left [not-yet-resolved], whether that landed on a real value or on
    /// missing/dangling.
    pub fn is_resolved(&self) -> bool {
```

(Reuse the existing doc comment above the method, promoting it from an
internal `//` note to a `///` doc comment since the method is now public —
adjust wording only enough to drop internal-only phrasing like "the reader
wires up real callers" if any leaked in from a neighboring comment; keep the
qpdf-relevant semantics.)

**Step 4: Run to verify it passes**

Run: `grep -n 'fn is_resolved' crates/flpdf/src/object_handle.rs`
Expected: `pub fn is_resolved`.
Run: `cargo build --workspace && cargo test -p flpdf --lib object_handle::`
Expected: clean build (no more `#[allow(dead_code)]`-style warnings need
touching here — `is_resolved` already has real in-crate callers in
`reader.rs`), all tests pass.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): promote is_resolved to public API"
```

---

### Task 5: `type_code`/`type_name`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`

qpdf's own `getTypeCode()`/`getTypeName()`
(`include/qpdf/QPDFObjectHandle.hh:311-315`,
`libqpdf/QPDFObjectHandle.cc:240-249`) silently trigger resolution
(`dereference()` calls `this->obj->resolve()`) before reading the type. This
port's `ObjectHandle` deliberately does not perform hidden I/O (design,
`Pdf` section: "Value access on an unresolved handle fails explicitly"), so
`type_code`/`type_name` report the qpdf-defined *unresolved*/*destroyed*
states instead of silently resolving — both are real entries in qpdf's own
`qpdf_object_type_e` (`include/qpdf/Constants.h:108-127`), not invented
here: `ot_uninitialized=0, ot_reserved=1, ot_null=2, ot_boolean=3,
ot_integer=4, ot_real=5, ot_string=6, ot_name=7, ot_array=8,
ot_dictionary=9, ot_stream=10, ot_operator=11, ot_inlineimage=12,
ot_unresolved=13, ot_destroyed=14`. `ot_uninitialized`/`ot_reserved` are
qpdf construction-time-only states this port's `ObjectHandle` never
occupies (every handle is fully constructed at birth) and are therefore
unreachable here — do not add dead branches for them.

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod type_code_tests {
    use super::*;

    #[test]
    fn direct_scalar_and_container_type_codes_match_qpdf_ordinals() {
        let cases: &[(ObjectHandle, u8, &str)] = &[
            (ObjectHandle::null(), 2, "null"),
            (ObjectHandle::boolean(true), 3, "boolean"),
            (ObjectHandle::integer(1), 4, "integer"),
            (ObjectHandle::real(1.5), 5, "real"),
            (ObjectHandle::real_literal(0.4, b".4".to_vec()), 5, "real"),
            (ObjectHandle::string(b"s".to_vec()), 6, "string"),
            (ObjectHandle::name(b"N".to_vec()), 7, "name"),
            (ObjectHandle::array(vec![]), 8, "array"),
            (ObjectHandle::dictionary(vec![]), 9, "dictionary"),
            (ObjectHandle::operator(b"q".to_vec()), 11, "operator"),
            (ObjectHandle::inline_image(b"d".to_vec()), 12, "inline-image"),
        ];
        for (handle, code, name) in cases {
            assert_eq!(handle.type_code(), *code, "{name}");
            assert_eq!(handle.type_name(), *name);
        }
    }

    #[test]
    fn stream_handle_type_code_is_stream() {
        let dict = ObjectHandle::dictionary(vec![]);
        let stream = ObjectHandle::from_value(ObjectValue::Stream {
            dict,
            data: Vec::new(),
        });
        assert_eq!(stream.type_code(), 10);
        assert_eq!(stream.type_name(), "stream");
    }

    #[test]
    fn not_yet_resolved_indirect_handle_reports_unresolved_without_resolving() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        assert_eq!(handle.type_code(), 13, "ot_unresolved");
        assert_eq!(handle.type_name(), "unresolved");
    }

    #[test]
    fn destroyed_indirect_handle_reports_destroyed() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(1));
        handle.disconnect();
        assert_eq!(handle.type_code(), 14, "ot_destroyed");
        assert_eq!(handle.type_name(), "destroyed");
    }

    #[test]
    fn missing_indirect_handle_reports_null_not_a_distinct_missing_code() {
        // qpdf has no separate "missing" ot_* code — a dangling/broken
        // reference presents as ot_null, matching set_missing's own
        // documented is_null()==true contract.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_missing();
        assert_eq!(handle.type_code(), 2, "ot_null");
        assert_eq!(handle.type_name(), "null");
    }

    #[test]
    fn resolved_indirect_handle_reports_its_real_value_type() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(1, 0), 0);
        handle.set_resolved(ObjectValue::Integer(7));
        assert_eq!(handle.type_code(), 4, "ot_integer");
        assert_eq!(handle.type_name(), "integer");
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::type_code_tests`
Expected: FAIL to compile — `type_code`/`type_name` don't exist yet.

**Step 3: Write the minimal implementation**

Avoid a nested `RefCell` borrow (`with_value` already borrows internally, so
do not additionally `.borrow()` the same slot around a `with_value` call —
compute both the special-cased states and the value-derived type in one
pass):

```rust
    /// The qpdf-compatible numeric type code of this handle's current known
    /// value: `include/qpdf/Constants.h:108-127`'s `qpdf_object_type_e`
    /// ordinals. An indirect handle that has not yet been resolved reports
    /// `13` (`ot_unresolved`) rather than performing hidden resolution; one
    /// whose owning document was dropped reports `14` (`ot_destroyed`) —
    /// both real qpdf states this port surfaces explicitly instead of
    /// qpdf's own silent-resolve-on-access behavior (design, `Pdf` section).
    pub fn type_code(&self) -> u8 {
        if let Repr::Indirect(slot) = &self.0 {
            // Bind the `Ref` guard to a local first, then match on `&slot.state`
            // — mirrors this file's own `Debug` impl (object_handle.rs:48-61),
            // rather than matching directly on the temporary borrow.
            let slot_ref = slot.borrow();
            match &slot_ref.state {
                IndirectState::NotYetResolved => return 13,
                IndirectState::Destroyed => return 14,
                IndirectState::Missing | IndirectState::Resolved(_) => {}
            }
        }
        self.with_value(|value| match value.expect("direct, missing, and resolved states all carry a value") {
            ObjectValue::Null => 2,
            ObjectValue::Boolean(_) => 3,
            ObjectValue::Integer(_) => 4,
            ObjectValue::Real(_) | ObjectValue::RealLiteral { .. } => 5,
            ObjectValue::String(_) => 6,
            ObjectValue::Name(_) => 7,
            ObjectValue::Array(_) => 8,
            ObjectValue::Dictionary(_) => 9,
            ObjectValue::Stream { .. } => 10,
            ObjectValue::Operator(_) => 11,
            ObjectValue::InlineImage(_) => 12,
            // A resolved redirect (ObjectValue::Reference) has no qpdf
            // ot_* analogue of its own — qpdf never observes an indirect
            // object whose resolved value is itself unresolved-looking;
            // this crate's Reference variant exists only for internal
            // redirect bookkeeping (see its own doc), so callers that reach
            // this state should treat it the same as the reference's real
            // eventual type is not knowable without following the chain via
            // `Pdf`. cov:ignore-reason: no production path resolves a
            // handle directly to Reference and then calls type_code on it
            // without first following the chain; exercised defensively.
            ObjectValue::Reference(_) => 13,
        })
    }

    /// The qpdf-compatible type name string for [`Self::type_code`]'s
    /// ordinal (`libqpdf/QPDFObjectHandle.cc:246-249`'s `getTypeName`, via
    /// each `QPDFObject` subclass's own name).
    pub fn type_name(&self) -> &'static str {
        match self.type_code() {
            2 => "null",
            3 => "boolean",
            4 => "integer",
            5 => "real",
            6 => "string",
            7 => "name",
            8 => "array",
            9 => "dictionary",
            10 => "stream",
            11 => "operator",
            12 => "inline-image",
            14 => "destroyed",
            _ => "unresolved", // cov:ignore: 13 is the only remaining reachable arm; `_` only for exhaustiveness
        }
    }
```

Before trusting the `ObjectValue::Reference` handling above, re-derive it
rather than keeping this plan's guess: grep whether any in-crate production
path (`reader.rs`) can leave a handle's `IndirectState::Resolved` holding
`ObjectValue::Reference` reachable from a public accessor call, or whether
`resolve_object_handle` always fully chases redirects before returning. If
it always chases, delete the `ObjectValue::Reference => 13` arm's
`cov:ignore` framing and instead make it `unreachable!()` with a comment
citing the exact `reader.rs` guarantee — do not leave speculative dead-code
handling in if the real invariant is stronger.

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib object_handle::`
Expected: all pass, including `type_code_tests`.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add qpdf-compatible type_code/type_name"
```

---

### Task 6: `unparse`/`unparse_resolved`

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`

qpdf's `unparse()` (`include/qpdf/QPDFObjectHandle.hh:1159`,
`libqpdf/QPDFObjectHandle.cc:1575-1583`): an indirect handle unparses to its
own `"N G R"`; a direct handle delegates to `unparseResolved()`.
`unparseResolved()` (`QPDFObjectHandle.cc:1585-1590`) dereferences (silent
resolve — this port does not) and returns the resolved value's own
`unparse()`. `QPDF_Stream::unparse()`
(`libqpdf/QPDF_Stream.cc:172-178`) always returns its own `"N G R"` — a
stream is only ever a top-level indirect object in valid qpdf usage — so
`unparse_resolved` on a stream value returns the same as `unparse`, not an
inlined dictionary+data.

This port's divergence from qpdf's own silent-resolve: `unparse_resolved`
on an indirect handle that has not yet been resolved delegates to
`materialize()`'s own already-documented behavior (returns `Object::Null`
without performing I/O — see `materialize`'s doc,
`object_handle.rs:524-538`), rather than triggering resolution. Record this
as a one-line comment on `unparse_resolved`, not a fresh design deviation —
it is the same no-hidden-I/O rule already established for every other
accessor in this file, just applied here too.

Implementation reuses the existing `pub(crate) materialize()` bridge and
`Object::write_pdf` for the actual byte-serialization, rather than
duplicating `Object::write_pdf`'s array/dict/string-escaping formatting
logic against `ObjectValue`. This is safe: `materialize()` already exists
in this exact file for this exact purpose (converting a fully-known
`ObjectHandle` value to `Object`), the byte formatting itself is already
byte-identical-tested via `Object::write_pdf`'s own suite, and this
internal call is not part of the public API surface (`unparse`/
`unparse_resolved`'s signatures return `Vec<u8>`, never `Object`). If a
later slice (`flpdf-egzr.3.2.8`, which deletes `Object` and `materialize`
entirely) needs a native `ObjectValue`-only serializer instead, that slice
reimplements this method's body against the by-then-final `ObjectValue`
shape — this task does not need to anticipate that rewrite.

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod unparse_tests {
    use super::*;

    #[test]
    fn direct_scalar_unparses_like_object_write_pdf() {
        assert_eq!(ObjectHandle::integer(7).unparse(), b"7");
        assert_eq!(ObjectHandle::boolean(true).unparse(), b"true");
        assert_eq!(ObjectHandle::name(b"Type".to_vec()).unparse(), b"/Type");
    }

    #[test]
    fn indirect_handle_unparse_is_always_the_reference_form_even_before_resolution() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        assert_eq!(handle.unparse(), b"7 2 R");
    }

    #[test]
    fn indirect_handle_unparse_resolved_falls_back_to_null_before_resolution() {
        // No hidden I/O: an unresolved indirect handle's value is not
        // known, so unparse_resolved reports the same as materialize()'s
        // own documented null fallback rather than triggering resolution.
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        assert_eq!(handle.unparse_resolved(), b"null");
    }

    #[test]
    fn resolved_indirect_handle_unparse_resolved_shows_the_real_value() {
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(7, 2), 0);
        handle.set_resolved(ObjectValue::Integer(42));
        assert_eq!(handle.unparse(), b"7 2 R");
        assert_eq!(handle.unparse_resolved(), b"42");
    }

    #[test]
    fn stream_value_unparse_resolved_still_reports_the_reference_form() {
        // QPDF_Stream::unparse() (libqpdf/QPDF_Stream.cc:172-178) always
        // returns its own "N G R" — mirrored here rather than inlining the
        // stream's dictionary/data.
        let dict = ObjectHandle::dictionary(vec![(b"Length".to_vec(), ObjectHandle::integer(0))]);
        let handle = ObjectHandle::new_indirect_unresolved(ObjectRef::new(9, 0), 0);
        handle.set_resolved(ObjectValue::Stream {
            dict,
            data: Vec::new(),
        });
        assert_eq!(handle.unparse(), b"9 0 R");
        assert_eq!(handle.unparse_resolved(), b"9 0 R");
    }

    #[test]
    fn direct_array_unparse_writes_indirect_children_as_references_not_recursed() {
        let child = ObjectHandle::new_indirect_unresolved(ObjectRef::new(5, 0), 0);
        let array = ObjectHandle::array(vec![ObjectHandle::integer(1), child]);
        assert_eq!(array.unparse(), b"[ 1 5 0 R ]");
    }
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --lib object_handle::unparse_tests`
Expected: FAIL to compile — `unparse`/`unparse_resolved` don't exist yet.

**Step 3: Write the minimal implementation**

```rust
    /// This handle's qpdf-syntax unparse form: an indirect handle always
    /// unparses to its own `"N G R"` (`include/qpdf/QPDFObjectHandle.hh:1159`),
    /// regardless of resolution state; a direct handle delegates to
    /// [`Self::unparse_resolved`].
    pub fn unparse(&self) -> Vec<u8> {
        match self.object_ref() {
            Some(object_ref) => {
                let mut out = Vec::new();
                Object::Reference(object_ref).write_pdf(&mut out);
                out
            }
            None => self.unparse_resolved(),
        }
    }

    /// This handle's resolved value in qpdf syntax
    /// (`libqpdf/QPDFObjectHandle.cc:1585-1590`), except a stream always
    /// reports its own reference form
    /// (`libqpdf/QPDF_Stream.cc:172-178`), and an indirect handle that has
    /// not yet been resolved reports the same as [`Self::materialize`]'s
    /// own null fallback rather than performing hidden resolution (design,
    /// `Pdf` section — no accessor in this crate resolves on the caller's
    /// behalf).
    pub fn unparse_resolved(&self) -> Vec<u8> {
        if self.object_ref().is_some()
            && self.with_value(|value| matches!(value, Some(ObjectValue::Stream { .. })))
        {
            return self.unparse();
        }
        let mut out = Vec::new();
        self.materialize().write_pdf(&mut out);
        out
    }
```

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --lib object_handle::`
Expected: all pass, including `unparse_tests`.

**Step 5: Commit**

```bash
git add crates/flpdf/src/object_handle.rs
git commit -m "feat(object_handle): add qpdf-compatible unparse/unparse_resolved"
```

---

### Task 7: Zero-consumer-diff verification (mandatory gate)

**This task is a gate, not new functionality. Do not skip it.**

**Step 1: Confirm the changed set matches the allowlist**

```bash
git diff --name-only main...HEAD -- crates/
```
Expected: `crates/flpdf/src/object_handle.rs`, and — only if a review
finding surfaced a population/decrypt-correctness gap underneath a new
accessor (see the "Post-implementation update" note near the top of this
plan) — `crates/flpdf/src/reader.rs`. flpdf-egzr.3.2.1's actual AC7 permits
exactly these two files and no others; any other line is a leak — stop, do
not proceed to Task 8, and revisit whichever of Tasks 2-6 (or the
`reader.rs` finding) introduced it.

**Step 2: Full workspace build and test, no features**

Run: `cargo build --workspace && cargo test --workspace`
Expected: clean build, all green (this also proves the new accessors don't
accidentally change any existing consumer's behavior, since none call them
yet).

**Step 3: No commit** (verification only).

---

### Task 8: Full regression — clippy, fmt, byte-identical, doctest

**Step 1: Format check**

Run: `cargo fmt --check`
Expected: no diff. If it fails, run `cargo fmt`, re-verify, and fold the fix
into this task's commit (per CLAUDE.md — do not create a separate "fmt"
commit).

**Step 2: Clippy, all features, warnings denied**

Run: `cargo clippy --workspace --all-features -- -D warnings`
Expected: clean.

**Step 3: Doctest**

Run: `cargo test --workspace --doc`
Expected: clean.

**Step 4: Full byte-identical suite (qpdf-zlib-compat), every invocation CI runs**

```bash
cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat --test deterministic_id_qpdf_parity_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_generate_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --lib overlay::byte_gate
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical
cargo test -p flpdf-cli --features qpdf-zlib-compat --test cli_byte_identical_overlay
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_baseline_static_id -- --nocapture
cargo test -p flpdf-cli --features qpdf-zlib-compat --test compat_matrix_baseline -- --nocapture
cargo test -p flpdf-qtest-tools --features qpdf-zlib-compat --test e2e
```
Expected: all green, byte-for-byte unchanged (re-verify the exact list
against `.github/workflows/ci.yml` at execution time — this plan's list is
a snapshot, not a promise the file hasn't grown a new entry since).

**Step 5: Full workspace test, all features**

Run: `cargo test --workspace --all-features`
Expected: all green.

**Step 6: Commit any fixes surfaced by this task**

```bash
git add -A
git commit -m "chore: fmt/clippy/doctest cleanup for ObjectHandle API surface"
```
(Only if Steps 1-3 required changes; skip if already clean.)

---

### Task 9: Coverage gate and qualitative review

**Step 1: Ensure all work is committed**

Run: `git status --short`
Expected: clean (patch-coverage errors on a dirty tree by design).

**Step 2: Read the current coverage script before trusting remembered flags**

Run: `sed -n '1,60p' scripts/patch-coverage.sh` — confirm current
`--base`/`--lcov`/`--allow-dirty` flag names before invoking it.

**Step 3: Run the coverage gate against main**

Run: `scripts/patch-coverage.sh --base main`
Expected: exit 0, 100% coverage on every changed line in `crates/flpdf`.
Any uncovered line: add a test, or (only if genuinely untestable) annotate
`// cov:ignore: <reason>` and record the reason in the PR description
(CLAUDE.md Test Coverage §2) — the `ObjectValue::Reference => 13` arm in
Task 5 is the most likely candidate; resolve per that task's own note
(prove reachability/unreachability first, don't default to `cov:ignore`).

**Step 4: Qualitative check (CLAUDE.md Test Coverage §4)**

Manually confirm — beyond the 100% line count — that real assertions exist
for: every new accessor's None-on-wrong-type case, every new accessor's
None-on-unresolved-indirect case, `type_code`/`type_name` across all five
indirect states (not-yet-resolved, resolved-to-real-value,
resolved-to-missing, resolved-to-reference, destroyed), and `unparse`
versus `unparse_resolved` divergence for both the stream case and the
not-yet-resolved case. These are the "accessor edge case newly reachable
through ObjectHandle" and "unparse normal vs resolved for at least one
indirect and one direct handle" clauses from this issue's acceptance
criteria — check each has a real test, not just a covered line.

**Step 5: No commit** (verification only, unless Step 3 required test
additions — then commit those with a normal `test:` message).

---

### Task 10: Update beads and prepare for PR

**Step 1: Record differential/verification commands on the issue**

`bd update flpdf-egzr.3.2.1 --notes "..."` — record the exact commands run
in Tasks 1, 8, and 9. Note AC5's *pinned-qpdf differential* clause: this
slice has no CLI-facing behavior change (zero consumer diff, verified in
Task 7), so there is nothing new to differential-test against a live
`qpdf` binary — record this as a scoped exception rather than silently
skipping AC5, the same way `flpdf-egzr.3.1`'s own notes recorded its AC5/AC7
exceptions.

**Step 2: Push and open the PR against `main`**

```bash
git push -u origin feat/flpdf-egzr-3-2-1-objecthandle-api
gh pr create --base main \
  --title "feat(flpdf): ObjectHandle + Pdf public handle API surface (flpdf-egzr.3.2.1)" \
  --body "$(cat <<'EOF'
## Summary
- Adds the ObjectHandle accessor surface external consumers (starting with
  flpdf-qtest-tools::driver::Handle in the next stack layer) need: Operator/
  InlineImage value representation, rounded-out scalar/reference accessors
  (as_boolean/as_real/as_name/as_string/as_operator/as_inline_image/
  as_reference), a public is_resolved, and qpdf-shaped type_code/type_name/
  unparse/unparse_resolved.
- Zero consumer files touched — verified by Task 7's `git diff --name-only`
  gate, which also permits `crates/flpdf/src/reader.rs` for a
  population/decrypt-correctness fix underneath the new accessors (two
  review findings; see the "Post-implementation update" note near the top
  of the plan); this is the first of 8 stacked sub-issues under
  flpdf-egzr.3.2.
- Design: docs/superpowers/specs/2026-07-30-xref-parsed-offset-object-handle-design.md
- Plan: docs/superpowers/plans/2026-07-31-objecthandle-consumer-api-surface.md

## Test plan
- [ ] cargo test --workspace (all features)
- [ ] Full qpdf-zlib-compat byte-identical suite (list in plan Task 8) - unchanged
- [ ] cargo clippy --workspace --all-features -- -D warnings
- [ ] cargo fmt --check
- [ ] scripts/patch-coverage.sh --base main - 100%
- [ ] git diff --name-only against main - crates/flpdf/src/object_handle.rs
      and crates/flpdf/src/reader.rs only (AC7)
EOF
)"
```

**Step 3: Session completion**

Follow CLAUDE.md's Session Completion protocol in full (`git pull --rebase`,
`bd dolt push`, `git push`, `git status` clean) once the PR is opened.

---

## Summary

This plan adds exactly the `ObjectHandle` surface the next 7 stack layers
need, confined to `object_handle.rs` (plus the narrow `reader.rs`
population/decrypt-correctness fix described in the "Post-implementation
update" note near the top — AC7 permits both), with zero behavior change to
any existing consumer. It closes a real representational gap (`Operator`/`InlineImage`),
rounds out the existing typed-accessor family to match `Object`'s own
accessor set, and ports the three qpdf-shaped operations
(`type_code`/`type_name`/`unparse`/`unparse_resolved`) the design doc names
explicitly — each grounded in a primary qpdf 11.9.0 source citation, not
guessed.

## Test plan

Every task's Step 1/Step 4 (RED/GREEN) plus Tasks 7-9's gates. No manual/UI
testing applies — this is a library-internal API surface with no CLI or
rendering path exercised directly by this plan.

## Non-goals (explicitly out of scope for this plan)

- Touching any consumer file (`writer.rs`, `page*.rs`, `json_inspect.rs`,
  `filters.rs`, CLI, qtest-tools) — that is `flpdf-egzr.3.2.2` through `.7`.
- Removing the legacy `Object` enum, `Pdf::resolve_borrowed`, or any
  clone-based resolution path — that is `flpdf-egzr.3.2.8`.
- Renaming `resolve_object_handle`/`get_all_object_handles`/`trailer_handle`
  onto `resolve`/`get_all_objects`/`trailer` — those legacy names are still
  occupied until `flpdf-egzr.3.2.8` deletes what currently holds them.
- `Pdf::get_xref_table()` — explicitly scoped to `flpdf-egzr.3.3` by the
  design doc's own Delivery Stack section.
- `array_item_indirectness`/`dictionary_items`-shaped convenience methods —
  composable from already-public `as_array`/`as_dictionary` plus per-child
  `is_indirect()`; not added since they earn no new capability.
- Any mutation entry point beyond what already exists
  (`set_resolved`/`set_missing`/`disconnect`/`replace_direct_value`/
  `reset_parsed_offset` stay `pub(crate)` — writer/page slices discover
  their own mutation needs when they migrate, not speculatively here).
