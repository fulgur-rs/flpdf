# Outline Action and Destination qpdf Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace flpdf's typed outline-action surface with qpdf 11.9.0-compatible raw destinations while retaining the existing `Dest`-based named-destination enumeration APIs.

**Architecture:** Keep eager `OutlineNode` materialization. Compute a raw `Object` destination once per node with qpdf's `/Dest` precedence, `/A /GoTo /D` gate, and Name-versus-String lookup split; keep the existing recursive `Dest` normalizer only for `legacy_dests()` and `name_tree_dests()`. Remove typed action and action-chain APIs after raw-object round-trip coverage is in place.

**Tech Stack:** Rust 2021, the existing `Pdf`/`Object` model and synthetic PDF test builder, qpdf 11.9.0 (`/usr/bin/qpdf` and `/tmp/qpdf-1190`) as oracle, `cargo llvm-cov` through `scripts/patch-coverage.sh`.

## Global Constraints

- Oracle behavior is qpdf 11.9.0, specifically `QPDFOutlineObjectHelper::getDest()`, `getDestPage()`, and `QPDFOutlineDocumentHelper::resolveNamedDest()`.
- Scope is outline action/destination only; do not change outline traversal, depth, direct-object handling, title/count semantics, page extraction, or writer/remapper algorithms.
- Keep public `Dest`, `Dest::page`, `legacy_dests() -> Result<Vec<(Vec<u8>, Option<Dest>)>>`, and `name_tree_dests() -> Result<Vec<(Vec<u8>, Option<Dest>)>>` unchanged.
- Remove `OutlineAction`, `OutlineNode::action`, both `action_chain` methods, and `DEFAULT_MAX_ACTION_CHAIN_DEPTH` immediately in the final API.
- `OutlineNode::dest` becomes `Object`; missing/unresolved destinations are `Object::Null`.
- Normal tests must not require qpdf. The explicit live-oracle test is ignored by default and run manually during verification.
- Every changed executable line in `crates/flpdf/src` must reach 100% patch coverage.
- Start implementation with `bd update flpdf-nm2o --claim`; close and push Beads only after all quality gates pass.

---

### Task 1: qpdf `getUTF8Value` String Decoder

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs:132-160`
- Test: `crates/flpdf/src/json_inspect.rs:2880-2975`

**Interfaces:**
- Consumes: existing `PDFDOC_ENCODING` and `lossy_utf16_to_utf8` in `json_inspect.rs`.
- Produces: `pub(crate) fn qpdf_utf8_value(bytes: &[u8]) -> String`, used by Task 2 for String named-destination lookup.

- [ ] **Step 1: Claim the Beads issue**

Run:

```bash
bd update flpdf-nm2o --claim
```

Expected: `flpdf-nm2o` is `IN_PROGRESS` and assigned to the current actor.

- [ ] **Step 2: Add failing decoder tests**

Add these unit tests beside the existing PDF string tests:

```rust
#[test]
fn qpdf_utf8_value_decodes_all_qpdf_string_encodings() {
    assert_eq!(qpdf_utf8_value(b"plain"), "plain");
    assert_eq!(qpdf_utf8_value(&[0xef, 0xbb, 0xbf, 0xe5, 0x90, 0x8d]), "名");
    assert_eq!(qpdf_utf8_value(&[0xfe, 0xff, 0x54, 0x0d, 0x52, 0x4d]), "名前");
    assert_eq!(qpdf_utf8_value(&[0x95]), "Ł");
}

#[test]
fn qpdf_utf8_value_replaces_undefined_pdfdoc_byte() {
    assert_eq!(qpdf_utf8_value(&[b'a', 0xad, b'b']), "a\u{fffd}b");
}
```

- [ ] **Step 3: Run the tests and verify the red state**

Run:

```bash
cargo test -p flpdf --lib qpdf_utf8_value -- --nocapture
```

Expected: compilation fails because `qpdf_utf8_value` does not exist.

- [ ] **Step 4: Implement the decoder**

Add this helper without changing `decode_pdf_text_string` behavior:

```rust
/// Match qpdf `QPDF_String::getUTF8Val`: UTF-16 BOM, explicit UTF-8 BOM,
/// otherwise PDFDocEncoding with U+FFFD for undefined entries.
pub(crate) fn qpdf_utf8_value(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return lossy_utf16_to_utf8(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return lossy_utf16_to_utf8(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(rest).into_owned();
    }

    bytes
        .iter()
        .map(|&byte| match byte {
            0x7f | 0x9f | 0xad => '\u{fffd}',
            _ => PDFDOC_ENCODING[byte as usize].unwrap_or(byte as char),
        })
        .collect()
}
```

- [ ] **Step 5: Run unit and formatting checks**

Run:

```bash
cargo test -p flpdf --lib qpdf_utf8_value -- --nocapture
cargo fmt --all -- --check
```

Expected: both decoder tests pass; formatting passes.

- [ ] **Step 6: Commit**

```bash
git add crates/flpdf/src/json_inspect.rs
git commit -m "feat(flpdf-nm2o): add qpdf PDF string decoder"
```

---

### Task 2: Raw `OutlineNode` Destination and qpdf Resolution

**Files:**
- Modify: `crates/flpdf/src/outline_document_helper.rs:78-124,330-530`
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs:320-540,1620-1810,2440-2810`
- Test: `crates/flpdf/tests/outline_pagelabels_e2e_tests.rs:110-145`

**Interfaces:**
- Consumes: `crate::json_inspect::qpdf_utf8_value`, `crate::ref_chain::resolve_ref_chain`, `read_name_tree`, and existing catalog helpers.
- Produces: `OutlineNode { pub dest: Object }`, `OutlineNode::dest_page(&self) -> Object`, and private node-only raw destination resolvers.
- Preserves: `Dest`, `dest_from_value`, `resolve_named_dest`, `legacy_dests`, and `name_tree_dests` for flpdf-specific normalized enumeration.

- [ ] **Step 1: Add the qpdf behavior matrix in a failing state**

Add source-near tests with explicit object expectations:

```rust
fn page_dest(page: u32) -> Object {
    Object::Array(vec![
        Object::Reference(ObjectRef::new(page, 0)),
        Object::Name(b"Fit".to_vec()),
    ])
}

fn qpdf_destination_matrix_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 7 0 R /Count 3 >>"),
            (5, "<< /Title (Array) /Parent 4 0 R /Next 6 0 R /A [10 0 R] >>"),
            (6, "<< /Title (Integer) /Parent 4 0 R /Prev 5 0 R /Next 7 0 R /Dest 42 /A << /S /GoTo /D [3 0 R /Fit] >> >>"),
            (7, "<< /Title (GoTo) /Parent 4 0 R /Prev 6 0 R /A << /S /GoTo /D [3 0 R /Fit] >> >>"),
            (10, "<< /S /GoTo /D [3 0 R /Fit] >>"),
        ],
        1,
    )
}

#[test]
fn qpdf_destination_matrix_matches_raw_objects() {
    let mut pdf = Pdf::open(Cursor::new(qpdf_destination_matrix_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    assert_eq!(roots[0].dest, Object::Null);
    assert_eq!(roots[1].dest, Object::Integer(42));
    assert_eq!(roots[2].dest, page_dest(3));
}

#[test]
fn dest_key_presence_suppresses_valid_action_fallback() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (P) /Parent 4 0 R /Dest 42 /A << /S /GoTo /D [3 0 R /Fit] >> >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(pdf.outline().get_root().unwrap()[0].dest, Object::Integer(42));
}

#[test]
fn root_action_array_is_not_an_action_dictionary() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (A) /Parent 4 0 R /A [10 0 R] >>"),
            (10, "<< /S /GoTo /D [3 0 R /Fit] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(pdf.outline().get_root().unwrap()[0].dest, Object::Null);
}

#[test]
fn candidate_type_selects_only_qpdf_named_destination_store() {
    let bytes = build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names 8 0 R /Dests 10 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 6 0 R /Count 2 >>"),
            (5, "<< /Title (Name) /Parent 4 0 R /Next 6 0 R /Dest /dup >>"),
            (6, "<< /Title (String) /Parent 4 0 R /Prev 5 0 R /Dest (dup) >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(dup) [3 0 R /Fit]] >>"),
            (10, "<< /dup [2 0 R /Fit] >>"),
        ],
        1,
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    assert_eq!(roots[0].dest, page_dest(2));
    assert_eq!(roots[1].dest, page_dest(3));
}
```

Add the remaining action gates as a compact table:

```rust
#[test]
fn malformed_or_non_goto_actions_have_null_destination() {
    for action in [
        "<< /S /GoTo >>",
        "<< /S 42 /D [3 0 R /Fit] >>",
        "<< /S /URI /D [3 0 R /Fit] >>",
        "<< /S /GoTo /D null >>",
        "<< /S /GoTo /SD [3 0 R /Fit] >>",
        "(not a dictionary)",
    ] {
        let mut pdf = Pdf::open(Cursor::new(action_pdf(action))).unwrap();
        assert_eq!(pdf.outline().get_root().unwrap()[0].dest, Object::Null);
    }
}
```

Add explicit named-destination shape and UTF-16 tests:

```rust
#[test]
fn unresolved_dest_name_suppresses_action_fallback() {
    let bytes = single_outline_with_catalog(
        "/Dests << >>",
        "/Dest /missing /A << /S /GoTo /D [3 0 R /Fit] >>",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(pdf.outline().get_root().unwrap()[0].dest, Object::Null);
}

#[test]
fn utf16_string_key_uses_qpdf_utf8_value() {
    let bytes = single_outline_with_catalog(
        "/Names << /Dests 8 0 R >>",
        "/Dest <FEFF540D524D>",
        &[(8, "<< /Names [<FEFF540D524D> [3 0 R /Fit]] >>")],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(pdf.outline().get_root().unwrap()[0].dest, page_dest(3));
}

#[test]
fn named_destination_preserves_dictionary_shape() {
    let bytes = single_outline_with_catalog(
        "/Dests << /dict << /D [3 0 R /Fit] >> >>",
        "/Dest /dict",
        &[],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    let node = &pdf.outline().get_root().unwrap()[0];
    assert!(matches!(node.dest, Object::Dictionary(_)));
    assert_eq!(node.dest_page(), Object::Null);
}

#[test]
fn named_destination_materializes_indirect_result_holder() {
    let bytes = single_outline_with_catalog(
        "/Dests << /held 8 0 R >>",
        "/Dest /held",
        &[(8, "[3 0 R /Fit]")],
    );
    let mut pdf = Pdf::open(Cursor::new(bytes)).unwrap();
    assert_eq!(pdf.outline().get_root().unwrap()[0].dest, page_dest(3));
}
```

Implement this test-only helper next to `action_pdf`; extra objects must use numbers greater than 5:

```rust
fn single_outline_with_catalog(
    catalog_entries: &str,
    item_entries: &str,
    extra: &[(u32, &str)],
) -> Vec<u8> {
    let mut owned = vec![
        (
            1,
            format!("<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R {catalog_entries} >>"),
        ),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string(),
        ),
        (
            4,
            "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>".to_string(),
        ),
        (5, format!("<< /Title (One) /Parent 4 0 R {item_entries} >>")),
    ];
    owned.extend(extra.iter().map(|(number, body)| (*number, (*body).to_string())));
    let borrowed: Vec<(u32, &str)> = owned
        .iter()
        .map(|(number, body)| (*number, body.as_str()))
        .collect();
    build_pdf(&borrowed, 1)
}
```

- [ ] **Step 2: Run the focused integration test and verify red**

Run:

```bash
cargo test -p flpdf --test outline_document_helper_tests dest_key_presence_suppresses_valid_action_fallback -- --exact
```

Expected: compilation fails because `OutlineNode::dest` is still `Option<Dest>`.

- [ ] **Step 3: Change the public node destination type**

Change only the node field and add the qpdf page accessor; keep `Dest` below it:

```rust
pub struct OutlineNode {
    // Existing fields unchanged.
    /// qpdf `getDest()` result; `Object::Null` means no resolved destination.
    pub dest: Object,
    // `se` and `children` unchanged.
}

impl OutlineNode {
    /// Mirror qpdf `getDestPage()` without resolving the page operand.
    pub fn dest_page(&self) -> Object {
        match &self.dest {
            Object::Array(items) if !items.is_empty() => items[0].clone(),
            _ => Object::Null,
        }
    }
}
```

- [ ] **Step 4: Implement qpdf candidate selection and raw lookup**

Replace the node's old `Option<Dest>` resolver with these boundaries. Keep the old `dest_from_value` and `resolve_named_dest` below for enumeration callers.

```rust
fn resolve_node_dest(
    &mut self,
    dest: Option<&Object>,
    action: Option<&Object>,
) -> Result<Object> {
    let candidate = if let Some(dest) = dest {
        Some(dest.clone())
    } else {
        self.goto_action_dest(action)?
    };
    match candidate {
        Some(value) => self.resolve_node_dest_value(value),
        None => Ok(Object::Null),
    }
}

fn goto_action_dest(&mut self, action: Option<&Object>) -> Result<Option<Object>> {
    let Some(action) = action else {
        return Ok(None);
    };
    let Object::Dictionary(dict) = resolve_terminal_object(self.pdf, action.clone())? else {
        return Ok(None);
    };
    let Some(subtype) = dict.get("S").cloned() else {
        return Ok(None);
    };
    let subtype = resolve_terminal_object(self.pdf, subtype)?;
    if !matches!(subtype, Object::Name(ref name) if name == b"GoTo") {
        return Ok(None);
    }
    Ok(dict.get("D").cloned())
}

fn resolve_node_dest_value(&mut self, value: Object) -> Result<Object> {
    match resolve_terminal_object(self.pdf, value)? {
        Object::Name(name) => self.resolve_legacy_node_dest(&name),
        Object::String(bytes) => self.resolve_name_tree_node_dest(&bytes),
        other => Ok(other),
    }
}

fn resolve_legacy_node_dest(&mut self, name: &[u8]) -> Result<Object> {
    let Some(Object::Dictionary(dests)) = self.catalog_value_terminal("Dests")? else {
        return Ok(Object::Null);
    };
    match dests.get(name).cloned() {
        Some(value) => resolve_terminal_object(self.pdf, value),
        None => Ok(Object::Null),
    }
}

fn resolve_name_tree_node_dest(&mut self, bytes: &[u8]) -> Result<Object> {
    let lookup = crate::json_inspect::qpdf_utf8_value(bytes);
    let Some(Object::Dictionary(mut names)) = self.catalog_value_terminal("Names")? else {
        return Ok(Object::Null);
    };
    let Some(dests_root) = names.remove("Dests") else {
        return Ok(Object::Null);
    };
    let entries = read_name_tree(
        self.pdf,
        dests_root,
        |_pdf, value| Ok(Some(value)),
        DEFAULT_MAX_OUTLINE_DEPTH,
    )?;
    for (stored, value) in entries {
        if crate::json_inspect::qpdf_utf8_value(&stored) == lookup {
            return resolve_terminal_object(self.pdf, value);
        }
    }
    Ok(Object::Null)
}
```

Add the free holder helper near the other resolution helpers:

```rust
fn resolve_terminal_object<R: Read + Seek>(pdf: &mut Pdf<R>, value: Object) -> Result<Object> {
    match value {
        value @ Object::Reference(_) => Ok(crate::ref_chain::resolve_ref_chain(pdf, &value)?.0),
        other => Ok(other),
    }
}
```

In `build_siblings`, retain `action_src` for the `/A /GoTo /D` fallback and existing typed action temporarily; only the `dest` return type changes in this task.

- [ ] **Step 5: Adapt existing node-destination assertions**

Apply these exact semantic replacements throughout both integration files:

```rust
assert_eq!(node.dest, page_dest(3));
assert_eq!(node.dest_page(), Object::Reference(ObjectRef::new(3, 0)));
assert_eq!(node_without_dest.dest, Object::Null);
```

Update prior recursive-normalization expectations to qpdf raw values:

```rust
// `/Dest 8 0 R`, where object 8 is `<< /D 8 0 R >>`.
assert!(matches!(roots[0].dest, Object::Dictionary(_)));

// Legacy `/Dests /mydest << /D [3 0 R /Fit] >>` remains a dictionary.
assert!(matches!(roots[0].dest, Object::Dictionary(_)));
assert_eq!(roots[0].dest_page(), Object::Null);

// `/Dests /a /b` returns `/b`; qpdf does not recursively follow aliases.
assert_eq!(roots[0].dest, Object::Name(b"b".to_vec()));
```

Do not alter assertions on `legacy_dests()` or `name_tree_dests()`; they must continue to use `Option<Dest>::page()`.

- [ ] **Step 6: Run focused and crate regression tests**

Run:

```bash
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf
cargo fmt --all -- --check
```

Expected: all pass. In particular, all named-destination enumeration and diagnostic tests remain green.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf/tests/outline_pagelabels_e2e_tests.rs
git commit -m "feat(flpdf-nm2o): match qpdf outline destinations"
```

---

### Task 3: Remove Typed Actions and Preserve Raw Round Trips

**Files:**
- Modify: `crates/flpdf/src/outline_document_helper.rs:61-177,330-370,700-750,1308-1501`
- Modify: `crates/flpdf/src/lib.rs:201-210`
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs:1620-2820`
- Test: `crates/flpdf/tests/outline_pagelabels_e2e_tests.rs:15-18,200-270,380-445`

**Interfaces:**
- Consumes: Task 2's raw `OutlineNode::dest` for GoTo assertions.
- Produces: final public API with no `OutlineAction`, node action field, action-chain methods, or action-chain depth constant.
- Preserves: raw `/A` and action `/Next` object graphs through `write_pdf` and page-remap tests.

- [ ] **Step 1: Replace typed round-trip coverage with raw-object coverage**

Add a helper that reads an outline item's raw `/A`:

```rust
fn raw_action(pdf: &mut Pdf<Cursor<Vec<u8>>>, item_ref: ObjectRef) -> Object {
    let Object::Dictionary(item) = pdf.resolve(item_ref).unwrap() else {
        panic!("outline item must be a dictionary");
    };
    item.get("A").cloned().unwrap_or(Object::Null)
}

fn resolved_raw_action(pdf: &mut Pdf<Cursor<Vec<u8>>>, item_ref: ObjectRef) -> Object {
    let mut value = raw_action(pdf, item_ref);
    let mut seen = BTreeSet::new();
    while let Object::Reference(reference) = value {
        assert!(seen.insert(reference), "cycle in test action holder");
        value = pdf.resolve(reference).unwrap();
    }
    value
}
```

Rewrite `action_round_trip_through_write_pdf_unmodified` to collect node refs first, then compare raw `/A` objects before and after `write_pdf`:

```rust
let refs: Vec<ObjectRef> = pdf
    .outline()
    .get_root()
    .unwrap()
    .into_iter()
    .map(|node| node.object_ref)
    .collect();
let before: Vec<Object> = refs.iter().map(|&r| raw_action(&mut pdf, r)).collect();
// write and reopen
let after: Vec<Object> = refs.iter().map(|&r| raw_action(&mut reopened, r)).collect();
assert_eq!(before, after);
```

Rewrite the e2e action-chain round trip to compare objects `10`, `11`, and `12` directly before and after writing, proving `/Next` survives without interpreting it.

- [ ] **Step 2: Rewrite remap tests to inspect raw action dictionaries**

For GoTo remap tests, assert Task 2's resolved node destination. For non-GoTo actions, resolve the raw action dictionary and inspect its fields:

```rust
let Object::Dictionary(action) = resolved_raw_action(&mut pdf, item_ref) else {
    panic!("/A must resolve to a dictionary");
};
assert_eq!(action.get("S"), Some(&Object::Name(b"GoToR".to_vec())));
assert_eq!(
    action.get("D").unwrap().as_array().unwrap()[0],
    Object::Reference(ObjectRef::new(30, 0))
);
```

Use equivalent raw assertions for URI, unknown subtype, and the combined e2e fixture. Keep the GoTo page-remap and named-destination remap tests; delete only assertions whose sole purpose was subtype classification.

- [ ] **Step 3: Remove typed-action and chain tests**

Delete:

- GoToR, URI, Launch, Named, and Unknown typed-classification-only tests;
- the entire source-near `/Next` action-chain walk section;
- Codex N7, N8, N10, and N11 typed/chain-only regressions.

Retain and rewrite as raw destination tests:

- direct/indirect GoTo `/D`;
- indirect and multi-hop `/A` holders;
- indirect `/S`;
- non-dictionary `/A`;
- `/D null`;
- `/SD` without `/D`, now expected to yield `Object::Null` exactly like qpdf.

- [ ] **Step 4: Remove the production action surface**

Delete from `outline_document_helper.rs`:

```rust
pub const DEFAULT_MAX_ACTION_CHAIN_DEPTH: usize = 100;
pub enum OutlineAction { /* all variants */ }
pub fn action_chain(/* ... */) -> Result<Vec<OutlineAction>>
pub fn action_chain_with_max_depth(/* ... */) -> Result<Vec<OutlineAction>>
fn parse_outline_action(/* ... */)
fn action_from_dict(/* ... */)
fn resolve_one_level_non_null(/* ... */)
fn is_file_spec(/* ... */)
fn collect_action_chain(/* ... */)
```

Remove `OutlineNode::action`, stop calling `parse_outline_action` in `build_siblings`, and remove the `action` initializer. Delete `resolve_one_level` too if `rg` shows no remaining caller after the Task 2 resolver uses `resolve_terminal_object`.

Change the `lib.rs` re-export to retain `Dest` but remove only action symbols:

```rust
pub use outline_document_helper::{
    check_legacy_dests, check_name_tree_dests, check_outline_links, prune_outline_se,
    prune_outline_se_with_max_depth, Dest, OutlineDocumentHelper, OutlineNode,
    MAX_OUTLINE_WALK_DEPTH,
};
```

- [ ] **Step 5: Prove the removed API has no residue**

Run:

```bash
rg -n "OutlineAction|DEFAULT_MAX_ACTION_CHAIN_DEPTH|pub fn action_chain|pub fn action_chain_with_max_depth|OutlineNode::action|node\.action|roots\[[^]]+\]\.action" crates/flpdf/src crates/flpdf/tests crates/flpdf-cli/src
```

Expected: no matches. `/Next` and internal action-chain handling may still appear in raw fixture strings and page-extraction code.

- [ ] **Step 6: Run focused, crate, and workspace tests**

Run:

```bash
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf
cargo test
cargo fmt --all -- --check
```

Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/src/lib.rs crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf/tests/outline_pagelabels_e2e_tests.rs
git commit -m "refactor(flpdf-nm2o)!: remove typed outline actions"
```

---

### Task 4: Live qpdf 11.9.0 Oracle Gate

**Files:**
- Modify: `crates/flpdf/Cargo.toml:24-27`
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs`
- Modify if dependency metadata changes: `Cargo.lock`

**Interfaces:**
- Consumes: Task 2's synthetic qpdf matrix and raw `Object` expectations.
- Produces: one ignored live-oracle test that proves the committed expectations against qpdf JSON without making normal tests require qpdf.

- [ ] **Step 1: Add `serde_json` as a test-only workspace dependency**

```toml
[dev-dependencies]
tempfile.workspace = true
flate2.workspace = true
serde_json.workspace = true
```

- [ ] **Step 2: Add the ignored qpdf oracle test**

Reuse the Task 2 fixture, write it to `tempfile::NamedTempFile`, and compare qpdf's outline JSON destinations:

```rust
#[test]
#[ignore = "live qpdf 11.9.0 oracle"]
fn qpdf_outline_destination_oracle_matches_expected_matrix() {
    use std::io::Write;
    use std::process::Command;

    let bytes = qpdf_destination_matrix_pdf();
    let mut input = tempfile::NamedTempFile::new().unwrap();
    input.write_all(&bytes).unwrap();

    let output = Command::new("qpdf")
        .args(["--json", "--json-key=outlines"])
        .arg(input.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let dests: Vec<serde_json::Value> = json["outlines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|outline| outline["dest"].clone())
        .collect();
    assert_eq!(
        dests,
        vec![
            serde_json::Value::Null,
            serde_json::json!(42),
            serde_json::json!(["3 0 R", "/Fit"]),
        ]
    );
}
```

Define `qpdf_destination_matrix_pdf()` once and use it in both the normal Rust expectations and this ignored test; its three top-level items are root `/A` array, `/Dest 42` with valid `/A`, and valid dictionary `/A /GoTo /D`.

- [ ] **Step 3: Run the oracle explicitly**

Run:

```bash
qpdf --version
cargo test -p flpdf --test outline_document_helper_tests qpdf_outline_destination_oracle_matches_expected_matrix -- --ignored --exact --nocapture
```

Expected: qpdf reports `11.9.0`; the ignored oracle test passes.

- [ ] **Step 4: Confirm normal tests do not invoke qpdf**

Run:

```bash
cargo test -p flpdf --test outline_document_helper_tests
```

Expected: all normal tests pass and the oracle test is reported as ignored.

- [ ] **Step 5: Commit**

```bash
git add crates/flpdf/Cargo.toml Cargo.lock crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "test(flpdf-nm2o): pin qpdf outline destination oracle"
```

If `Cargo.lock` is unchanged, omit it from `git add`.

---

### Task 5: CI Quality, 100% Patch Coverage, and Delivery

**Files:**
- Modify only if coverage identifies a real missed branch: `crates/flpdf/tests/outline_document_helper_tests.rs` or `crates/flpdf/src/json_inspect.rs` test module
- Beads state: `flpdf-nm2o`

**Interfaces:**
- Consumes: all implementation commits from Tasks 1-4.
- Produces: CI-equivalent local evidence, 100% changed-line coverage, closed/pushed Beads state, and a pushed git branch.

- [ ] **Step 1: Run the complete quality gate**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test -p flpdf --test outline_document_helper_tests
cargo test -p flpdf --test outline_pagelabels_e2e_tests
cargo test -p flpdf
cargo test
```

Expected: every command exits zero.

- [ ] **Step 2: Run the authoritative patch-coverage gate**

Run after Tasks 1-4 are committed:

```bash
scripts/patch-coverage.sh --base main
```

Expected: the `crates/flpdf` changed-line report is 100% and the command exits zero.

- [ ] **Step 3: Cover any reported branch before proceeding**

The planned matrix must exercise every resolver branch: `/Dest` present, `/Dest` absent, action absent, action non-dict, `/S` absent/non-GoTo/GoTo, `/D` absent/present, candidate Name/String/other, both lookup catalogs absent/present, lookup miss/hit, direct/indirect holders, `dest_page` array/non-array/empty, all three string decoder prefixes, PDFDocEncoding, and undefined-byte replacement.

If the report names a missed line, add the corresponding missing case from that list, rerun its focused test, commit with:

```bash
git add crates/flpdf/tests/outline_document_helper_tests.rs crates/flpdf/src/json_inspect.rs
git commit -m "test(flpdf-nm2o): complete destination branch coverage"
scripts/patch-coverage.sh --base main
```

Expected: the second coverage run reports 100%. Do not use `cov:ignore` unless the line is genuinely unreachable or an llvm-cov artifact, and document that reason inline.

- [ ] **Step 4: Verify scope and working tree**

Run:

```bash
git diff --stat origin/main..HEAD
git status --short --branch
bd show flpdf-nm2o
```

Expected: only the design/plan, `json_inspect.rs`, `outline_document_helper.rs`, `lib.rs`, the two outline integration tests, `crates/flpdf/Cargo.toml`, optional `Cargo.lock`, and Beads-managed state are in scope; the worktree is clean; `flpdf-nm2o` is still in progress.

- [ ] **Step 5: Close and push Beads**

Run:

```bash
bd close flpdf-nm2o --reason "qpdf 11.9.0 action/destination parity implemented; typed action APIs removed; tests and 100% patch coverage pass"
bd dolt push
```

Expected: issue is closed and Dolt push completes.

- [ ] **Step 6: Push git and verify remote tracking**

Run:

```bash
git push
git status --short --branch
```

Expected: push succeeds and the branch is synchronized with its upstream.
