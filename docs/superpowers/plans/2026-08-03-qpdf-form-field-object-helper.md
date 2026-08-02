# QPDFFormFieldObjectHelper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `form_field_object_helper.rs` the sole qpdf 11.9.0 form-field boundary, migrate its consumers, and delete the duplicated field logic.

**Architecture:** A new helper owns form-field traversal, typed values, mutation, and the public appearance entry point. `AnnotationObjectHelper` retains annotation-only accessors, while `appearance.rs` retains only rendering primitives called by the helper. The implementation follows `QPDFFormFieldObjectHelper.hh/.cc` method groups and verifies every semantic decision against pinned qpdf 11.9.0.

**Tech Stack:** Rust workspace; `Pdf`, `ObjectHandle`, `ObjectRef`; pinned qpdf 11.9.0 source; cargo tests and patch coverage.

---

## File structure

- Create `crates/flpdf/src/form_field_object_helper.rs`: qpdf form-field public API, inherited traversal, mutation, and appearance orchestration.
- Modify `crates/flpdf/src/annotation_helper.rs`: retain `AnnotationObjectHelper`; remove `FormFieldObjectHelper` and its field-only walkers.
- Modify `crates/flpdf/src/appearance.rs`: make rendering/allocation helpers crate-private and remove the three public field entry points and duplicated inheritance walkers.
- Modify `crates/flpdf/src/lib.rs`: declare and re-export the new helper; stop re-exporting deleted field and appearance APIs.
- Modify `crates/flpdf/src/acroform_document_helper.rs` and `crates/flpdf/src/signatures.rs`: import the new helper module.
- Modify `crates/flpdf/tests/annotation_helper_tests.rs` and `crates/flpdf/tests/annotation_helper_error_tests.rs`: move form-field coverage to the new integration test.
- Create `crates/flpdf/tests/form_field_object_helper_tests.rs`: public API and qpdf behavior regression coverage.
- Modify `crates/flpdf/src/appearance.rs` unit tests: exercise crate-private primitives only, with public behavior moved to the integration test.

### Task 1: Inventory and oracle matrix

**Files:**
- Create: `docs/superpowers/plans/2026-08-03-qpdf-form-field-object-helper-oracle.md`
- Test: `crates/flpdf/tests/form_field_object_helper_tests.rs`

- [ ] **Step 1: Record every qpdf public method and its Rust destination**

Create a table with one row each for `isNull`, parent/top-level traversal, inheritable getters, names, values/defaults, resources/appearance/quadding/flags, predicates/choices, setters, and `generateAppearance`. Cite the matching declaration in `include/qpdf/QPDFFormFieldObjectHelper.hh` and definition in `libqpdf/QPDFFormFieldObjectHelper.cc`.

- [ ] **Step 2: Add a compile-failing public API test**

```rust
use flpdf::form_field_object_helper::FormFieldObjectHelper;

#[test]
fn exposes_qpdf_form_field_helper_from_its_own_module() {
    let _ = std::any::type_name::<FormFieldObjectHelper<'static, std::io::Cursor<Vec<u8>>>>();
}
```

- [ ] **Step 3: Run the test to verify the missing module boundary**

Run: `cargo test -p flpdf --test form_field_object_helper_tests exposes_qpdf_form_field_helper_from_its_own_module`

Expected: FAIL because `form_field_object_helper` does not exist yet.

- [ ] **Step 4: Commit the oracle matrix and RED test**

```bash
git add docs/superpowers/plans/2026-08-03-qpdf-form-field-object-helper-oracle.md crates/flpdf/tests/form_field_object_helper_tests.rs
git commit -m "test: define FormFieldObjectHelper oracle matrix"
```

### Task 2: Establish the read-only helper boundary

**Files:**
- Create: `crates/flpdf/src/form_field_object_helper.rs`
- Modify: `crates/flpdf/src/annotation_helper.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/tests/form_field_object_helper_tests.rs`

- [ ] **Step 1: Add failing inheritance, naming, and typed-value tests**

Cover a child field with indirect `/FT`, `/V`, `/DV`, `/Ff`, `/T`, `/TU`, and `/TM` values; verify qpdf-shaped type names (including leading `/`), fully-qualified/alternative/mapping fallback order, and parent-cycle termination. Cover a non-dictionary field returning qpdf null/empty semantics rather than panicking.

- [ ] **Step 2: Run the focused RED tests**

Run: `cargo test -p flpdf --test form_field_object_helper_tests inherited_`

Expected: FAIL because the new helper has no implementation.

- [ ] **Step 3: Implement the new helper and migrate existing reads**

Move the existing field-only resolver from `annotation_helper.rs` into `form_field_object_helper.rs`, then implement this public surface:

```rust
pub fn is_null(&mut self) -> Result<bool>;
pub fn parent(&mut self) -> Result<Option<ObjectRef>>;
pub fn top_level_field(&mut self) -> Result<(ObjectRef, bool)>;
pub fn inheritable_value(&mut self, key: &[u8]) -> Result<Option<Object>>;
pub fn field_type(&mut self) -> Result<Option<Vec<u8>>>;
pub fn fully_qualified_name(&mut self) -> Result<String>;
pub fn partial_name(&mut self) -> Result<String>;
pub fn alternative_name(&mut self) -> Result<String>;
pub fn mapping_name(&mut self) -> Result<String>;
pub fn value(&mut self) -> Result<Option<Object>>;
pub fn default_value(&mut self) -> Result<Option<Object>>;
pub fn flags(&mut self) -> Result<i64>;
```

Resolve scalar values through indirect references before type testing, preserve raw PDF names where the qpdf contract exposes names, and retain the existing malformed-chain cycle/depth error behavior where it is stricter than qpdf's silent null result.

- [ ] **Step 4: Remove the old field helper from `annotation_helper.rs`**

Delete `FormFieldObjectHelper`, its field-only imports, docs, and private inherited resolvers from `annotation_helper.rs`. Keep `AnnotationObjectHelper` and its annotation-only tests unchanged.

- [ ] **Step 5: Run focused tests**

Run: `cargo test -p flpdf --test form_field_object_helper_tests && cargo test -p flpdf --test annotation_helper_tests`

Expected: PASS.

- [ ] **Step 6: Commit the read boundary**

```bash
git add crates/flpdf/src/form_field_object_helper.rs crates/flpdf/src/annotation_helper.rs crates/flpdf/src/lib.rs crates/flpdf/tests/form_field_object_helper_tests.rs crates/flpdf/tests/annotation_helper_tests.rs crates/flpdf/tests/annotation_helper_error_tests.rs
git commit -m "feat: add qpdf FormFieldObjectHelper read boundary"
```

### Task 3: Complete field metadata and type predicates

**Files:**
- Modify: `crates/flpdf/src/form_field_object_helper.rs`
- Test: `crates/flpdf/tests/form_field_object_helper_tests.rs`

- [ ] **Step 1: Add failing tests for AcroForm fallback and field classifications**

Build an `/AcroForm` with indirect `/DA`, `/DR`, and `/Q`; assert field-level inheritance wins, then AcroForm fallback applies. Test `is_text`, `is_checkbox`, `is_checked`, `is_radio_button`, `is_pushbutton`, `is_choice`, and `/Opt` choice extraction for direct and indirect values.

- [ ] **Step 2: Run the metadata RED tests**

Run: `cargo test -p flpdf --test form_field_object_helper_tests metadata_`

Expected: FAIL because these APIs do not exist.

- [ ] **Step 3: Implement the qpdf metadata group**

Add `default_resources`, `default_appearance`, `quadding`, predicate methods, and `choices`. `default_resources` reads only document `/AcroForm/DR`; `default_appearance` and `quadding` first use field inheritance, then document fallback. Decode qpdf text strings only for APIs whose qpdf counterpart returns UTF-8 text.

- [ ] **Step 4: Run the metadata tests**

Run: `cargo test -p flpdf --test form_field_object_helper_tests metadata_`

Expected: PASS.

- [ ] **Step 5: Commit metadata support**

```bash
git add crates/flpdf/src/form_field_object_helper.rs crates/flpdf/tests/form_field_object_helper_tests.rs
git commit -m "feat: complete form field metadata accessors"
```

### Task 4: Move mutation and appearance orchestration

**Files:**
- Modify: `crates/flpdf/src/form_field_object_helper.rs`
- Modify: `crates/flpdf/src/appearance.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/tests/form_field_object_helper_tests.rs`

- [ ] **Step 1: Add failing mutation and appearance tests**

Test `set_field_attribute` with a Unicode string, `set_value` for text/choice with `/NeedAppearances`, checkbox on/off state selection, radio button child `/AS` updates, pushbutton no-op behavior, and `generate_appearance` dispatch for `/Tx` and `/Ch` only. Use existing appearance fixtures so the assertions check resulting `/V`, `/AS`, `/AP`, and `/NeedAppearances` objects.

- [ ] **Step 2: Run the mutation RED tests**

Run: `cargo test -p flpdf --test form_field_object_helper_tests set_ && cargo test -p flpdf --test form_field_object_helper_tests generate_`

Expected: FAIL because the helper has no mutation or appearance methods.

- [ ] **Step 3: Implement qpdf-owned mutation paths**

Add `set_field_attribute`, `set_value`, checkbox/radio private helpers, and `generate_appearance`. Port qpdf's `/Btn` dispatch: non-name checkbox/radio inputs do not mutate; pushbuttons do not mutate; checkbox values choose the first non-`/Off` normal-appearance state and update `/AS`; radio values update matching child widgets. For text and choice values, write qpdf-style Unicode strings and set document `/AcroForm/NeedAppearances` only when requested.

- [ ] **Step 4: Reduce `appearance.rs` to rendering primitives**

Move only the public field orchestration out of `appearance.rs`; keep pure drawing, stream allocation, and font lookup functions crate-private with explicit helper inputs. Remove the three public `generate_*_field_appearance` re-exports from `lib.rs` and update internal tests to call the new public helper instead.

- [ ] **Step 5: Run mutation and appearance suites**

Run: `cargo test -p flpdf --test form_field_object_helper_tests && cargo test -p flpdf appearance::tests`

Expected: PASS.

- [ ] **Step 6: Commit mutation and appearance ownership**

```bash
git add crates/flpdf/src/form_field_object_helper.rs crates/flpdf/src/appearance.rs crates/flpdf/src/lib.rs crates/flpdf/tests/form_field_object_helper_tests.rs
git commit -m "feat: move form field mutation and appearance ownership"
```

### Task 5: Cut over production consumers and delete duplicated routes

**Files:**
- Modify: `crates/flpdf/src/acroform_document_helper.rs`
- Modify: `crates/flpdf/src/signatures.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`
- Modify: `crates/flpdf-cli/src/main.rs`
- Modify: `crates/flpdf/src/annotation_helper.rs`
- Modify: `crates/flpdf/src/appearance.rs`
- Test: `crates/flpdf/tests/form_field_object_helper_tests.rs`

- [ ] **Step 1: Add consumer integration regressions**

Add one public helper test proving an indirect `/FT` survives through the AcroForm and signature consumers, one JSON inspection assertion preserving qpdf's leading slash in a type name, and one CLI regression confirming generated appearances clear `/NeedAppearances` only through the new helper-owned path.

- [ ] **Step 2: Run the consumer RED tests**

Run: `cargo test -p flpdf --test form_field_object_helper_tests consumer_ && cargo test -p flpdf-cli --test cli_tests appearance`

Expected: FAIL until consumers import the new helper and the legacy routes are removed.

- [ ] **Step 3: Cut over every call site found by the Task 1 inventory**

Change `acroform_document_helper.rs`, `signatures.rs`, `json_inspect.rs`, and CLI code to call `FormFieldObjectHelper` from its new module. Do not retain re-export aliases or duplicate inherited walkers. Preserve annotation-only behavior in `annotation_helper.rs` and low-level drawing in `appearance.rs`.

- [ ] **Step 4: Prove legacy removal**

Run: `rg -n 'struct FormFieldObjectHelper|generate_(text|button|choice)_field_appearance|resolve_inherited_(name|object|integer)' crates/flpdf/src`

Expected: only `form_field_object_helper.rs` contains form-field traversal or public field orchestration; no compatibility wrapper remains.

- [ ] **Step 5: Run consumer regressions**

Run: `cargo test -p flpdf --test form_field_object_helper_tests && cargo test -p flpdf-cli --test cli_tests appearance`

Expected: PASS.

- [ ] **Step 6: Commit the cutover**

```bash
git add crates/flpdf/src/acroform_document_helper.rs crates/flpdf/src/signatures.rs crates/flpdf/src/json_inspect.rs crates/flpdf-cli/src/main.rs crates/flpdf/src/annotation_helper.rs crates/flpdf/src/appearance.rs crates/flpdf/tests/form_field_object_helper_tests.rs
git commit -m "refactor: cut consumers over to FormFieldObjectHelper"
```

### Task 6: Differential verification and completion gate

**Files:**
- Modify: `crates/flpdf/tests/form_field_object_helper_tests.rs`
- Modify: `docs/superpowers/specs/2026-08-03-qpdf-form-field-object-helper-design.md`

- [ ] **Step 1: Add real-qpdf probes for unresolved semantics**

Use tiny synthetic PDFs to compare qpdf 11.9.0 behavior for indirect field scalars, `/DA` and `/Q` AcroForm fallback, button state changes, and `/NeedAppearances`. Record each command, exit status, and observed output in the test comments or design spec beside its matching regression.

- [ ] **Step 2: Run all focused suites**

Run: `cargo test -p flpdf --test form_field_object_helper_tests && cargo test -p flpdf --test annotation_helper_tests && cargo test -p flpdf --test annotation_helper_error_tests && cargo test -p flpdf --lib appearance::tests && cargo test -p flpdf-cli --test cli_tests appearance`

Expected: PASS.

- [ ] **Step 3: Run workspace quality gates and coverage**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace && scripts/patch-coverage.sh --base origin/main`

Expected: every command exits 0 and patch coverage reports zero uncovered changed lines.

- [ ] **Step 4: Commit verification evidence**

```bash
git add crates/flpdf/tests/form_field_object_helper_tests.rs docs/superpowers/specs/2026-08-03-qpdf-form-field-object-helper-design.md
git commit -m "test: verify qpdf FormFieldObjectHelper parity"
```
