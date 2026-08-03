# Field/widget appearance boundary Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Match qpdf appearance generation by binding a helper to a field and passing its widget explicitly.

**Architecture:** `FormFieldObjectHelper` owns inherited field state. A new explicit-widget API sends a terminal field reference plus widget reference to crate-private rendering; only the widget receives `/AP` updates.

**Tech Stack:** Rust, flpdf `Pdf`/`ObjectRef`, qpdf 11.9.0, cargo.

## Global Constraints

- qpdf 11.9.0 `generateAppearance(QPDFAnnotationObjectHelper&)` is authoritative.
- Remove, rather than wrap, `generate_appearance()` and `generate_button_appearance()`.
- Start every semantic change with a focused failing test.
- Pass format, focused suites, workspace tests, clippy, and changed-line coverage.

---

### Task 1: Add field/widget API and regression

**Files:**
- Modify: `crates/flpdf/src/form_field_object_helper.rs:383-401`
- Modify: `crates/flpdf/tests/form_field_object_helper_tests.rs`

**Produces:** `generate_appearance_for(&mut self, widget_ref: ObjectRef) -> Result<Option<ObjectRef>>`.

- [ ] **Step 1: Write a failing field/widget test**

```rust
let mut helper = FormFieldObjectHelper::new(ObjectRef::new(10, 0), &mut pdf);
assert!(helper.generate_appearance_for(ObjectRef::new(11, 0)).unwrap().is_some());
assert!(pdf.resolve(ObjectRef::new(11, 0)).unwrap().as_dict().unwrap().contains_key("AP"));
```

Use field 10 with `/FT /Tx`, `/V`, and no `/Rect`; use widget 11 with `/Rect` and no `/V`.

- [ ] **Step 2: Verify RED**

Run `cargo test -p flpdf --test form_field_object_helper_tests generates_a_field_value_on_its_separate_widget`.

Expected: method does not exist.

- [ ] **Step 3: Implement the replacement API**

```rust
pub fn generate_appearance_for(&mut self, widget_ref: ObjectRef) -> Result<Option<ObjectRef>> {
    match self.field_type()?.as_deref() {
        Some(b"/Tx") => rendering::render_text_field(self.pdf, self.field_ref, widget_ref),
        Some(b"/Ch") => rendering::render_choice_field(self.pdf, self.field_ref, widget_ref),
        _ => Ok(None),
    }
}
```

Delete the old no-argument public methods.

- [ ] **Step 4: Verify GREEN and commit**

Run `cargo test -p flpdf --test form_field_object_helper_tests generates_a_field_value_on_its_separate_widget`.

Commit with `git commit -m "refactor: separate field and widget appearance API"`.

### Task 2: Migrate renderer and CLI consumers

**Files:**
- Modify: `crates/flpdf/src/form_field_object_helper/rendering.rs`
- Modify: `crates/flpdf-cli/src/main.rs:3375-3383`
- Modify: `crates/flpdf/tests/form_field_object_helper_tests.rs`

**Consumes:** Task 1 API.

- [ ] **Step 1: Add a failing assertion**

Assert the terminal field stays without `/AP` and the explicit widget obtains `/AP`.

- [ ] **Step 2: Verify RED**

Run the Task 1 test; expect rendering still reads geometry or writes appearance from the field reference.

- [ ] **Step 3: Split renderer inputs**

Change text/choice renderer signatures to `(pdf, field_ref, widget_ref)`. Read inherited `/FT`, `/V`, `/DA`, `/DR`, `/Q`, `/Ff`, and `/Opt` from `field_ref`; read `/Rect` and write `/AP` only at `widget_ref`. Update the CLI widget traversal to find its terminal field and call `generate_appearance_for(widget_ref)`.

- [ ] **Step 4: Verify and commit**

Run:

```bash
cargo test -p flpdf --test form_field_object_helper_tests
cargo test -p flpdf --lib form_field_object_helper::rendering::tests
cargo test -p flpdf-cli --test cli_tests appearance
rg 'generate_appearance\(|generate_button_appearance\(' crates
```

Expected: tests pass and only `generate_appearance_for` remains.

Commit with `git commit -m "refactor: render appearances through field widget pairs"`.

### Task 3: Close review regressions and coverage

**Files:**
- Modify: `crates/flpdf/tests/form_field_object_helper_tests.rs`

- [ ] **Step 1: Add remaining review regressions**

Cover a child `/V` reference resolving to null followed by an ancestor reference, UTF-16BE names, direct-before-indirect checkbox widgets, and the separated field/widget appearance path.

- [ ] **Step 2: Verify all gates**

Run:

```bash
cargo fmt -- --check
cargo clippy -p flpdf --all-targets --all-features -- -D warnings
cargo test --workspace
scripts/patch-coverage.sh --base origin/main
git diff --check
```

For a branch provably unreachable after a public invariant, use a local `// cov:ignore: <reason>` only.

- [ ] **Step 3: Commit**

Commit with `git commit -m "test: cover form field helper review cases"`.
