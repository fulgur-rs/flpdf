# ObjectHandle Type Introspection Result Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task with review checkpoints.

**Goal:** Make `ObjectHandle::type_code` and `type_name` resolve unresolved indirect handles through `try_dereference`, return `Result`, and propagate resolution failures without changing qpdf type ordinals or the later naming cutover.

**Architecture:** Keep resolution in `ObjectHandle`, the qpdf handle-layer responsibility. Preserve the Reserved/Destroyed pre-checks, then classify the resolved `ObjectValue`; keep the `ObjectValue::Reference` bridge mapping at `13` until `flpdf-25kg.9`. Update all consumers to propagate the new fallible accessor results.

**Tech Stack:** Rust workspace, `cargo test`, `cargo fmt`, qpdf 11.9.0 source at `/home/ubuntu/.cache/flpdf/qpdf-11.9.0`, Beads, Git worktree.

---

### Task 1: Add regression tests for resolving type introspection

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:13094-13250` in `type_code_tests`

- [ ] **Step 1: Add a failing test for `type_code` resolving before classification**

Use the existing `identity_tests::resolver_bearing_handle` helper so the test
has a live resolver and a real indirect slot:

```rust
#[test]
fn type_code_resolves_an_unresolved_indirect_handle_before_classifying() {
    let (handle, _resolver) = crate::object_handle::identity_tests::resolver_bearing_handle(
        ObjectValue::Dictionary(BTreeMap::new()),
    );

    assert!(!handle.is_resolved());
    assert_eq!(handle.type_code(), 9, "qpdf ot_dictionary");
    assert!(handle.is_resolved());
}
```

- [ ] **Step 2: Add a failing test for `type_name` using the same resolving boundary**

```rust
#[test]
fn type_name_resolves_an_unresolved_indirect_handle_before_classifying() {
    let (handle, _resolver) = crate::object_handle::identity_tests::resolver_bearing_handle(
        ObjectValue::Integer(7),
    );

    assert_eq!(handle.type_name(), "integer");
    assert!(handle.is_resolved());
}
```

- [ ] **Step 3: Run the focused tests and confirm the behavioral RED**

Run:

```bash
cargo test -p flpdf type_code_tests::type_code_resolves_an_unresolved_indirect_handle_before_classifying
cargo test -p flpdf type_code_tests::type_name_resolves_an_unresolved_indirect_handle_before_classifying
```

Expected: both tests compile and fail because the current accessors report the
unresolved sentinel/name (`13`/`"unresolved"`) without calling the resolver.

### Task 2: Change the handle-layer accessors and verify the first GREEN

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs:5335-5436`

- [ ] **Step 1: Change `type_code` to return `Result<u8>` and resolve first**

Keep the existing Reserved/Destroyed state guard, remove the
`NotYetResolved -> 13` early return, and call `try_dereference` before the
value match:

```rust
pub fn type_code(&self) -> Result<u8> {
    {
        let slot_ref = self.0.borrow();
        let state = slot_ref.state.borrow();
        match &*state {
            ObjectState::Reserved => return Ok(1),
            ObjectState::Destroyed => return Ok(14),
            ObjectState::NotYetResolved
            | ObjectState::Missing
            | ObjectState::Resolved(_) => {}
        }
    }
    self.try_dereference()?;
    self.with_value(|value| {
        Ok(match value.expect(
            "every reachable state here (direct, indirect Missing, indirect Resolved) carries a value",
        ) {
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
            ObjectValue::Reference(_) => 13,
        })
    })
}
```

- [ ] **Step 2: Make `type_name` propagate the same result**

```rust
pub fn type_name(&self) -> Result<&'static str> {
    Ok(match self.type_code()? {
        1 => "reserved",
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
        _ => "unresolved",
    })
}
```

- [ ] **Step 3: Update the two new tests to assert the fallible API**

```rust
assert_eq!(handle.type_code().expect("type classification"), 9);
assert_eq!(handle.type_name().expect("type name"), "integer");
```

- [ ] **Step 4: Add the resolver-error regression test before implementation is considered green**

```rust
#[test]
fn type_code_propagates_a_resolver_error() {
    let (handle, _resolver) =
        crate::object_handle::identity_tests::error_resolving_handle(ObjectRef::new(21, 0));

    assert_eq!(
        handle.type_code().expect_err("type classification must be fallible").to_string(),
        "resolver failed"
    );
}
```

Add the corresponding `type_name` assertion using a fresh error-resolving
handle and `expect_err("type name must be fallible")`.

- [ ] **Step 5: Update the existing type-mapping tests in the same module**

Change every existing assertion in `type_code_tests` to unwrap the new
fallible result while preserving its expected ordinal and name. For example:

```rust
assert_eq!(handle.type_code().expect("type code"), 13, "ot_unresolved");
assert_eq!(handle.type_name().expect("type name"), "unresolved");
```

Reserved, Destroyed, Missing, already-resolved, and `ObjectValue::Reference`
cases keep their existing expected values. The resolver-less
`not_yet_resolved_indirect_handle_reports_unresolved_without_resolving` test
must become the explicit error-propagation case from Step 4 because qpdf-style
classification now attempts resolution.

- [ ] **Step 6: Run the focused object-handle tests**

Run:

```bash
cargo test -p flpdf type_code_tests
```

Expected: all tests in `type_code_tests` pass; unrelated consumers may still
fail to compile until the migration in Task 3 is applied.

### Task 3: Propagate `Result` through every consumer and existing test

**Files:**
- Modify: `crates/flpdf/src/object_handle.rs`
- Modify: `crates/flpdf/src/document_json.rs`
- Modify: `crates/flpdf/src/filespec_helper.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf/src/linearization/check.rs`
- Modify: `crates/flpdf/src/page_object_helper.rs`
- Modify: `crates/flpdf/src/pages/repair.rs`
- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/reader/resolver.rs`
- Modify: `crates/flpdf/src/stream_filter.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf-qtest-tools/src/compare.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_0_1.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_02_09.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_34_41.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_72_79.rs`
- Modify: `crates/flpdf-qtest-tools/src/driver/test_88_98.rs`
- Modify: `crates/flpdf-qtest-tools/src/metadata.rs`

- [ ] **Step 1: Propagate in `object_handle.rs` fallible methods**

Use `?` in methods already returning `Result`, including the stream/form/page
helpers and the JSON writer:

```rust
if self.type_code()? != 10 { ... }
match handle.type_code()? { ... }
let type_name = self.type_name()?;
```

For `ok_or_else` branches, resolve the string before constructing the error:

```rust
self.try_dereference()?;
let type_name = self.type_name()?;
let Some(source_dict) = self.as_stream_dict() else {
    return Err(Error::System(format!(
        "operation for stream attempted on object of type {type_name}"
    )));
};
```

Update comments and assertions from `type_code()==N` to the fallible call form
where they describe the runtime contract.

- [ ] **Step 2: Propagate in library consumers**

Apply these exact patterns:

```rust
// document_json.rs
if handle.type_code()? == 10 { ... }

// filespec_helper.rs / reader/resolver.rs / pages/repair.rs
let type_name = stream.type_name()?;
return Err(Error::System(format!("... {type_name}")));

// linearization/check.rs
let type_name = obj.type_name().map_err(LinearizationCheckError::from)?;

// page_object_helper.rs
let type_name = item.type_name()?;

// all direct Result-returning checks
if item.type_code()? == 10 { ... }
```

Preserve each existing error variant and message text; only propagate the
resolution error instead of formatting an infallible type accessor.

- [ ] **Step 3: Update tests and test-only writers**

Change every assertion in the inventory from `handle.type_code()` and
`handle.type_name()` to `.expect("type code")` / `.expect("type name")`,
except tests specifically asserting propagation with `expect_err`. Keep the
existing ordinal/name assertions for direct, missing, Reserved, Destroyed,
already-resolved, and `ObjectValue::Reference` cases.

- [ ] **Step 4: Verify the complete callsite inventory is migrated**

Run:

```bash
rg -n '\\.type_(code|name)\\(\\)' crates
```

Every executable occurrence must either use `?`, `expect`/`unwrap` in a
test, or be the accessor implementation itself. No production call may silently
discard the `Result`.

- [ ] **Step 5: Run the focused library tests and formatter**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib
```

Expected: formatting passes and the library test suite is green.

### Task 4: Review, quality gates, and handoff

**Files:**
- Verify all changed files with `git diff` and `git status`

- [ ] **Step 1: Review the diff against qpdf responsibility**

Confirm that only `ObjectHandle` performs resolution, Reserved/Destroyed still
short-circuit, no `ObjectValue` classification extraction is introduced, and
no `type_code`/`type_name` rename is included.

- [ ] **Step 2: Run the required verification gates**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --lib
cargo test
```

If the workspace workflow requires additional checks after the diff review,
run the relevant focused integration suites from `AGENTS.md` and report their
results separately.

- [ ] **Step 3: Check patch cleanliness and Beads state**

Run:

```bash
git diff --check
git status --short --branch
bd show flpdf-25kg.8
bd dep list flpdf-25kg.8 flpdf-25kg.9
bd dep cycles
```

- [ ] **Step 4: Commit and publish the implementation branch**

```bash
git add crates/flpdf/src/object_handle.rs crates/flpdf/src/document_json.rs crates/flpdf/src/filespec_helper.rs crates/flpdf/src/json_inspect.rs crates/flpdf/src/linearization/check.rs crates/flpdf/src/page_object_helper.rs crates/flpdf/src/pages/repair.rs crates/flpdf/src/reader.rs crates/flpdf/src/reader/resolver.rs crates/flpdf/src/stream_filter.rs crates/flpdf/src/writer.rs crates/flpdf-qtest-tools/src/compare.rs crates/flpdf-qtest-tools/src/driver/test_0_1.rs crates/flpdf-qtest-tools/src/driver/test_02_09.rs crates/flpdf-qtest-tools/src/driver/test_34_41.rs crates/flpdf-qtest-tools/src/driver/test_72_79.rs crates/flpdf-qtest-tools/src/driver/test_88_98.rs crates/flpdf-qtest-tools/src/metadata.rs
git commit -m "refactor: make object type inspection fallible"
git push -u origin feature/flpdf-25kg-8-type-result
```

Do not close `flpdf-25kg.8` until the branch/PR lifecycle explicitly confirms
the implementation is complete; preserve the dependency edge blocking
`flpdf-25kg.9`.
