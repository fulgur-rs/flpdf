# Destroyed Handle `is_null` Implementation Plan

**Goal:** Make a handle disconnected by `Pdf::drop` report non-null and make
the stream-filter reader reject it as an invalid `/Filter`, matching qpdf 11.9.0.

**Architecture:** Keep `IndirectState::Destroyed` and `with_value` unchanged.
Teach only `ObjectHandle::is_null` to inspect an indirect slot's actual state:
literal null and `Missing` remain null; `NotYetResolved` and `Destroyed` do
not. The existing real-`Pdf` stream-filter regression becomes the end-to-end
proof of both the accessor and consumer behavior.

**Tech Stack:** Rust, `flpdf`, qpdf 11.9.0 source as semantic oracle.

## Constraints

- Work only in `feat/flpdf-nrp3-destroyed-is-null`.
- Preserve `with_value`'s Destroyed fallback and all missing-reference semantics.
- Do not add a sentinel, panic, public API, or error type.
- Track task state in Beads; this document is implementation guidance, not a tracker.

## Files

| File | Responsibility |
|---|---|
| `crates/flpdf/src/stream_filter.rs` | End-to-end regression for a resolved `/Filter` retained after `Pdf::drop`. |
| `crates/flpdf/src/object_handle.rs` | qpdf-compatible `is_null` state classification. |

## Execution

### 1. Establish the red regression

Modify `handle_reader_reads_a_filter_disconnected_by_pdf_teardown_as_absent`
in `crates/flpdf/src/stream_filter.rs` and rename it to
`handle_reader_rejects_a_filter_disconnected_by_pdf_teardown`.

After the live control assertion and `drop(pdf)`, assert:

```rust
assert!(filter.is_resolved(), "disconnect leaves a terminal state");
assert_eq!(filter.type_code(), 14, "qpdf ot_destroyed");
assert!(!filter.is_null());
let error = decode_filter_specs_from_handle(&filter, &ObjectHandle::null(), None)
    .expect_err("a destroyed /Filter is not absent");
assert_eq!(
    error.to_string(),
    "unsupported PDF feature: stream filter type is not name or array"
);
```

Replace the obsolete comment that calls the behavior a known divergence with a
qpdf citation for `QPDFObjectHandle::isNull` and `QPDF_Stream::filterable`.

In `crates/flpdf/src/object_handle.rs`, rename
`disconnect_replaces_a_resolved_value_and_presents_as_null` to
`disconnect_replaces_a_resolved_value_with_destroyed_state`, replace its
`assert!(handle.is_null())` with `assert!(!handle.is_null())`, and add
`assert_eq!(handle.type_code(), 14)`.

Run:

```bash
cargo test -p flpdf handle_reader_rejects_a_filter_disconnected_by_pdf_teardown
cargo test -p flpdf disconnect_replaces_a_resolved_value_with_destroyed_state
```

Expected before implementation: both commands fail at their new
`assert!(!handle.is_null())` assertion.

### 2. Make the smallest accessor correction

In `crates/flpdf/src/object_handle.rs`, replace `is_null`'s `with_value`
fallback with a direct `Repr`/`IndirectState` match:

```rust
match &self.0 {
    Repr::Direct(slot) => matches!(&slot.borrow().value, ObjectValue::Null),
    Repr::Indirect(slot) => matches!(
        &slot.borrow().state,
        IndirectState::Resolved(ObjectValue::Null) | IndirectState::Missing
    ),
}
```

This leaves `with_value` unchanged, so its documented Destroyed fallback stays
confined to accessors that already rely on it.

Re-run the same focused command. Expected: pass.

### 3. Verify the surrounding contracts

Run:

```bash
cargo test -p flpdf disconnect_replaces_a_resolved_value_with_destroyed_state
cargo test -p flpdf --test reader_tests
cargo fmt --all -- --check
```

Preserve the existing literal-null and missing-reference tests unchanged. Then
run the full workspace suite:

```bash
cargo test --workspace
```

Finally inspect `git diff --check` and the scoped diff before committing the
two Rust files and the approved design/plan records.
