# qtest type-checks QPDFObjectHandle Port Design

**Goal:** Make every subtest in qpdf 11.9.0 `type-checks.test` pass by porting the complete `QPDFObjectHandle` type-check behavior exercised by `test_driver 42` into the canonical Rust handle layer.

**Scope:** The current qtest run passes cases 1, 2, 5, and 6. Cases 3 and 4 fail because `run_test_42` stops after its initial setup and emits no type-operation warnings. This design covers the qpdf `test_42` operation sequence and its ordinary-PDF and ObjStm observations; it does not change qpdf fixtures or unrelated test-driver IDs.

## Authority and observed gap

The pinned qpdf 11.9.0 source is authoritative:

- `qpdf/test_driver.cc:1407-1549` defines the complete `test_42` operation order and assertions.
- `include/qpdf/QPDFObjectHandle.hh:597-637` defines the warning contract: wrong-type value access returns a zero-like value, out-of-range array access returns null, and wrong-type mutation is ignored; each fallback warns when the handle has a document context.
- `libqpdf/QPDFObjectHandle.cc:240-330` performs dereference-before-type inspection.
- `libqpdf/QPDFObjectHandle.cc:502-740,759-785,856-1023,1199-1248` owns the scalar, array, dictionary, and mutator behavior.
- `libqpdf/QPDFObjectHandle.cc:2168-2212` owns `typeWarning`, `warnIfPossible`, and `objectWarning` formatting and context behavior.
- `libqpdf/QPDFObjectHandle.cc:789-853,1987-2002` owns Rectangle/Matrix predicates, fallback values, and constructors.

The isolated run against a writable copy of `vendor/qpdf-qtest` reports:

```text
type-checks 1 ... PASSED
type-checks 2 ... PASSED
type-checks 3 ... FAILED
type-checks 4 ... FAILED
type-checks 5 ... PASSED
type-checks 6 ... PASSED
```

Cases 3 and 4 differ only because the current Rust driver emits `test 42 done` while qpdf emits the 43 warning/error lines recorded by `object-types.out` and `object-types-os.out`.

## Chosen approach

Three approaches were considered:

1. Emit the expected warning strings directly from `run_test_42`. This would make the fixture pass but would create a driver-only semantic shim and would not test the core object contract.
2. Add one opaque “run type checks” helper to flpdf. This would hide several independently testable qpdf responsibilities behind a test-specific entry point.
3. Port the exercised `QPDFObjectHandle` surface once in `object_handle.rs`, then make `run_test_42` a thin consumer. This preserves qpdf ownership and lets ordinary callers observe the same warning/fallback semantics.

Approach 3 is selected. No expected output, qpdf fixture, or warning text is copied into production or synthesized by the driver.

## Core ObjectHandle boundary

Extend `crates/flpdf/src/object_handle.rs` with the qpdf-owned operations needed by `test_42`, preserving the existing canonical resolver and `DiagnosticOrigin::Object` warning path.

### Accessors and fallback behavior

Expose fallible Rust facades for qpdf's warning-producing accessors. The Rust `Result` is retained at the existing resolution and warning-sink failure boundary; a successful wrong-type call returns qpdf's fallback value.

- bool, integer, real, name, string, UTF-8 string, operator, inline-image, and numeric accessors warn and return qpdf's zero-like fallback.
- array length, array item, and array-vector accessors warn on a non-array receiver; array item also emits an object warning for an invalid index and returns a direct null handle.
- dictionary `has_key`, `get_key`, `get_key_if_dict`, `get_keys`, and `get_dict_as_map` retain qpdf's distinction between a missing-key null description and a non-dictionary type warning.
- Array and dictionary mutators keep qpdf's dereference-before-type-check order, ignore wrong-type mutations after warning, and emit object warnings for invalid array indices.
- Existing silent `as_*` and raw map accessors remain the no-warning inspection family. The warning-producing family must not be implemented by calling a silent accessor and returning early.

Array-index conversion must not use a sentinel to represent absence. The qpdf-facing operation accepts a signed index so `-1` and large positive indices follow the same explicit bounds branch as qpdf; existing internal index-based consumers remain on their current checked `usize` paths where their domain is already proven.

### Geometry helpers

Add ObjectHandle-owned Rectangle and Matrix helpers corresponding to qpdf's nested convenience types. Rectangle uses the existing zero-default `flpdf::Rectangle`. Matrix needs a distinct qpdf ObjectHandle matrix value with an all-zero default, because the existing public `flpdf::Matrix` is the affine identity by default and changing that would alter an unrelated API.

- `is_rectangle` and `is_matrix` resolve the receiver and inspect exact length plus numeric children without producing type warnings.
- `get_array_as_rectangle` and `get_array_as_matrix` return qpdf's zero-default fallback for a non-array, wrong length, or non-numeric child.
- Constructors produce canonical direct array handles and preserve numeric values through the existing real/integer ObjectValue representation.

### Iterators and initialized state

Port the exercised array/dictionary iterator behavior at the ObjectHandle boundary rather than replacing it with a test-only vector walk. Iterator end values are represented as an explicit uninitialized iterator value, not as a null object. Add an ObjectHandle initialized-state query and preserve the existing unresolved/reserved/destroyed states as distinct from uninitialized.

The default/uninitialized state must fail at qpdf's dereference boundary with the existing Rust error classification, while `is_initialized`, `is_integer`, `is_dictionary`, and `is_scalar` remain non-warning queries. No empty handle, null handle, or numeric sentinel is used to stand for uninitialized state.

## qtest driver integration

Replace the `run_test_42` GAP in `crates/flpdf-qtest-tools/src/driver/test_42_49.rs` with the qpdf operation sequence in source order:

1. Resolve `/QTest`, `/Dictionary`, `/Key2`, and `/Integer` through canonical handles.
2. Exercise array and dictionary iterators and their end-state assertions.
3. Invoke every wrong-type accessor/mutator and geometry helper from qpdf `test_42`.
4. Emit the driver's `One error` and `Two errors` markers at the same points as qpdf.
5. Drain new `Pdf` diagnostics immediately after each warning-producing operation using the existing `emit_new_diagnostics` boundary, preserving object descriptions, offsets, and output ordering.
6. Keep the stream-dictionary check and all qpdf assertions, then return the existing `test 42 done` completion path from the outer driver.

The driver will use only public canonical APIs from `flpdf`; it will not call `pub(crate)` warning helpers, materialize raw objects merely to trigger warnings, or print expected warning literals.

## Testing and acceptance

Add source-near unit tests for each new ObjectHandle family before implementation code is considered complete:

- warning context and exact message composition for parsed indirect objects;
- zero-like scalar fallbacks and wrong-type array/dictionary accessors;
- valid and invalid signed array indices, including `-1`;
- ignored wrong-type and out-of-bounds mutations;
- nested child descriptions and warning order;
- Rectangle/Matrix valid and fallback values;
- iterator end-state and uninitialized-handle behavior.

The driver test must assert the exact stderr/stdout bytes for both `object-types.pdf` and `object-types-os.pdf`. Verification then runs the focused Rust tests, the writable-copy `type-checks.test`, the workspace format/build/test/clippy/rustdoc/qpdf checks, and a full qtest run using the same `harness.log` and `qtest-results.xml` pair. Only after that pair proves the rows passing will `flpdf-qtest/parity/qtest-11.9.0.jsonl` promote `type-checks 3` and `type-checks 4`; unrelated rows remain unchanged.

## Non-goals

- Do not change qpdf-qtest fixtures or expected outputs.
- Do not add a driver-only compatibility or warning shim.
- Do not implement unrelated `QPDFObjectHandle` APIs from other test-driver IDs.
- Do not change the existing `flpdf::Matrix` identity default.
- Do not alter the current qtest manifest until the exact same-run evidence exists.
