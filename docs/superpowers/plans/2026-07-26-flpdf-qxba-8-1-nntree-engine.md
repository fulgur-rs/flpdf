# NNTree Shared Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:executing-plans` to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port qpdf 11.9.0's shared NNTree iterator, lookup, mutation, splitting, and repair engine without switching production consumers.

**Architecture:** Add one crate-private `nntree.rs` engine parameterized by a name or number key codec. A cursor stores qpdf's path and item position; node mutation uses an explicit root/indirect anchor plus direct-`/Kids` path so direct children can be updated without unsafe shared-object emulation. Existing `name_number_tree.rs`, consumer modules, and public exports remain unchanged in this layer.

**Tech Stack:** Rust 2021 with MSRV 1.87; existing `Pdf`, `Object`, `Dictionary`, `ObjectRef`, `Error`, `resolve_ref_chain`, and diagnostics APIs; qpdf 11.9.0 source and `libtests/nntree.cc` as the oracle; no new dependencies.

## Global Constraints

- qpdf 11.9.0 is the behavioral oracle; resolve it with `scripts/fetch-qpdf-source.sh --print-path`.
- Do not edit the fetched qpdf worktree.
- Do not change existing production consumers in this layer.
- Do not add `unsafe`, global interior mutability, or new traversal/split budgets.
- The default split threshold is exactly 32.
- Preserve qpdf warning text, warning order, split order, object allocation order, and root `/Limits` omission.
- Use TDD: every production behavior starts with a focused test that fails for the intended missing behavior.
- Keep existing user changes and the separate `flpdf-qxba.6` worktree untouched.
- Final committed `HEAD` must have 100% changed-line coverage against `origin/main`.

## File map

- Create `crates/flpdf/src/nntree.rs`: crate-private shared engine, codecs, cursor, node access, lookup, mutation, split, and repair.
- Modify `crates/flpdf/src/lib.rs`: declare `mod nntree;` without public re-exports in this layer.
- Add tests inside `crates/flpdf/src/nntree.rs`: engine-only access is intentional until typed public helpers land in `flpdf-qxba.8.2`.
- Do not modify `name_number_tree.rs`, `name_tree_dests.rs`, or any consumer.

---

### Task 1: Module, key codecs, and node storage

**Files:**
- Create: `crates/flpdf/src/nntree.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Test: `crates/flpdf/src/nntree.rs`

**Interfaces:**
- Consumes: `crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result}` and `crate::ref_chain::resolve_ref_chain`.
- Produces:

```rust
pub(crate) const DEFAULT_SPLIT_THRESHOLD: usize = 32;

pub(crate) trait TreeKey {
    type Key: Clone + std::fmt::Debug + Eq + Ord;
    const ITEMS_KEY: &'static str;
    fn from_object(object: &Object) -> Option<Self::Key>;
    fn to_object(key: &Self::Key) -> Object;
    fn compare(left: &Self::Key, right: &Self::Key) -> std::cmp::Ordering;
}

pub(crate) enum NameKey {}
pub(crate) enum NumberKey {}

pub(crate) struct NNTree<K: TreeKey> {
    root: Object,
    auto_repair: bool,
    split_threshold: usize,
    marker: std::marker::PhantomData<K>,
}
```

- Produces internal `NodeHandle`, `load_node`, `store_node`, `make_indirect`, `warn`, and `structural_error` helpers used by every later task.

- [ ] **Step 1: Add the missing-module RED test**

Add this declaration to `crates/flpdf/src/lib.rs`:

```rust
mod nntree;
```

Run:

```sh
cargo check -p flpdf
```

Expected: FAIL with `file not found for module nntree`.

- [ ] **Step 2: Add codec tests before codec production code**

Create `crates/flpdf/src/nntree.rs` with imports and tests only:

```rust
//! Mirrors qpdf 11.9.0 libqpdf/NNTree.cc.
//! Public wrappers correspond to QPDFNameTreeObjectHelper and
//! QPDFNumberTreeObjectHelper and are added in the next stacked layer.

use crate::ref_chain::resolve_ref_chain;
use crate::{Dictionary, Error, Object, ObjectRef, Pdf, Result};
use std::cmp::Ordering;
use std::fmt::Debug;
use std::io::{Read, Seek};
use std::marker::PhantomData;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_number_codecs_accept_only_their_qpdf_key_types() {
        assert_eq!(
            NameKey::from_object(&Object::String(b"alpha".to_vec())),
            Some(b"alpha".to_vec())
        );
        assert_eq!(NameKey::from_object(&Object::Integer(1)), None);
        assert_eq!(
            NumberKey::from_object(&Object::Integer(-7)),
            Some(-7)
        );
        assert_eq!(NumberKey::from_object(&Object::String(b"7".to_vec())), None);
    }

    #[test]
    fn codec_round_trip_preserves_pdf_key_objects() {
        assert_eq!(
            NameKey::to_object(&b"a\0z".to_vec()),
            Object::String(b"a\0z".to_vec())
        );
        assert_eq!(NumberKey::to_object(&42), Object::Integer(42));
    }
}
```

Run:

```sh
cargo test -p flpdf nntree::tests::name_and_number_codecs_accept_only_their_qpdf_key_types
```

Expected: FAIL because `NameKey`, `NumberKey`, and `TreeKey` are undefined.

- [ ] **Step 3: Implement the codecs**

Add:

```rust
pub(crate) const DEFAULT_SPLIT_THRESHOLD: usize = 32;

pub(crate) trait TreeKey {
    type Key: Clone + Debug + Eq + Ord;
    const ITEMS_KEY: &'static str;

    fn from_object(object: &Object) -> Option<Self::Key>;
    fn to_object(key: &Self::Key) -> Object;

    fn compare(left: &Self::Key, right: &Self::Key) -> Ordering {
        left.cmp(right)
    }
}

pub(crate) enum NameKey {}

impl TreeKey for NameKey {
    type Key = Vec<u8>;
    const ITEMS_KEY: &'static str = "Names";

    fn from_object(object: &Object) -> Option<Self::Key> {
        match object {
            Object::String(value) => Some(value.clone()),
            _ => None,
        }
    }

    fn to_object(key: &Self::Key) -> Object {
        Object::String(key.clone())
    }
}

pub(crate) enum NumberKey {}

impl TreeKey for NumberKey {
    type Key = i64;
    const ITEMS_KEY: &'static str = "Nums";

    fn from_object(object: &Object) -> Option<Self::Key> {
        match object {
            Object::Integer(value) => Some(*value),
            _ => None,
        }
    }

    fn to_object(key: &Self::Key) -> Object {
        Object::Integer(*key)
    }
}
```

Run both codec tests. Expected: PASS.

- [ ] **Step 3a: Add the exact shared test fixtures**

Keep all engine fixtures in `tests` and use these definitions throughout the
remaining tasks:

```rust
use std::io::Cursor;

type TestPdf = Pdf<Cursor<Vec<u8>>>;

fn empty_pdf() -> TestPdf {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.4\n");
    let off1 = bytes.len() as u64;
    bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
    let xref = bytes.len() as u64;
    bytes.extend_from_slice(
        format!(
            "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \n\
             trailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    Pdf::open(Cursor::new(bytes)).expect("open")
}

fn name_leaf(entries: &[(&[u8], i64)], limits: Option<(&[u8], &[u8])>) -> Object {
    let mut dictionary = Dictionary::new();
    let items = entries
        .iter()
        .flat_map(|(key, value)| {
            [Object::String(key.to_vec()), Object::Integer(*value)]
        })
        .collect();
    dictionary.insert("Names", Object::Array(items));
    if let Some((first, last)) = limits {
        dictionary.insert(
            "Limits",
            Object::Array(vec![
                Object::String(first.to_vec()),
                Object::String(last.to_vec()),
            ]),
        );
    }
    Object::Dictionary(dictionary)
}

fn number_leaf(entries: &[(i64, &[u8])], limits: Option<(i64, i64)>) -> Object {
    let mut dictionary = Dictionary::new();
    let items = entries
        .iter()
        .flat_map(|(key, value)| {
            [Object::Integer(*key), Object::String(value.to_vec())]
        })
        .collect();
    dictionary.insert("Nums", Object::Array(items));
    if let Some((first, last)) = limits {
        dictionary.insert(
            "Limits",
            Object::Array(vec![Object::Integer(first), Object::Integer(last)]),
        );
    }
    Object::Dictionary(dictionary)
}

fn root_with_one_direct_leaf() -> Object {
    let mut root = Dictionary::new();
    root.insert(
        "Kids",
        Object::Array(vec![name_leaf(&[(b"a", 1)], Some((b"a", b"a")))]),
    );
    Object::Dictionary(root)
}

fn two_leaf_number_tree(pdf: &mut TestPdf) -> Object {
    let left_ref = ObjectRef::new(10, 0);
    let right_ref = ObjectRef::new(11, 0);
    pdf.set_object(
        left_ref,
        number_leaf(&[(10, b"ten"), (20, b"twenty")], Some((10, 20))),
    );
    pdf.set_object(
        right_ref,
        number_leaf(&[(30, b"thirty"), (40, b"forty")], Some((30, 40))),
    );
    let mut root = Dictionary::new();
    root.insert(
        "Kids",
        Object::Array(vec![
            Object::Reference(left_ref),
            Object::Reference(right_ref),
        ]),
    );
    Object::Dictionary(root)
}

fn collect_number_entries(
    tree: &mut NNTree<NumberKey>,
    pdf: &mut TestPdf,
) -> Vec<(i64, Object)> {
    let mut entries = Vec::new();
    let mut cursor = tree.begin(pdf).expect("begin");
    while let Some((key, value)) = cursor.cloned_current() {
        entries.push((key, value));
        tree.next(pdf, &mut cursor).expect("next");
    }
    entries
}

fn collect_name_entries(
    tree: &mut NNTree<NameKey>,
    pdf: &mut TestPdf,
) -> Vec<(Vec<u8>, Object)> {
    let mut entries = Vec::new();
    let mut cursor = tree.begin(pdf).expect("begin");
    while let Some((key, value)) = cursor.cloned_current() {
        entries.push((key, value));
        tree.next(pdf, &mut cursor).expect("next");
    }
    entries
}

fn warning_messages(pdf: &TestPdf) -> Vec<&str> {
    pdf.repair_diagnostics()
        .entries()
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect()
}

fn number_tree_shape(pdf: &mut TestPdf, object: &Object) -> String {
    let resolved = match object {
        Object::Reference(object_ref) => pdf.resolve(*object_ref).expect("resolve node"),
        object => object.clone(),
    };
    let Object::Dictionary(dictionary) = resolved else {
        panic!("tree node must be a dictionary");
    };
    if let Some(Object::Array(items)) = dictionary.get("Nums") {
        let keys = items
            .chunks_exact(2)
            .map(|pair| match pair[0] {
                Object::Integer(key) => key.to_string(),
                ref other => panic!("unexpected number-tree key: {other:?}"),
            })
            .collect::<Vec<_>>()
            .join(",");
        return format!("L[{keys}]");
    }
    let Object::Array(kids) = dictionary.get("Kids").expect("node shape") else {
        panic!("/Kids must be an array");
    };
    let children = kids
        .iter()
        .map(|kid| number_tree_shape(pdf, kid))
        .collect::<Vec<_>>()
        .join(",");
    format!("K({children})")
}

fn malformed_name_tree_with_missing_limits_and_valid_pairs(pdf: &mut TestPdf) -> Object {
    let leaf_ref = ObjectRef::new(10, 0);
    pdf.set_object(
        leaf_ref,
        name_leaf(&[(b"alpha", 1), (b"beta", 2)], None),
    );
    let mut root = Dictionary::new();
    root.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
    Object::Dictionary(root)
}
```

- [ ] **Step 4: Add node-handle RED tests**

Add:

```rust
#[test]
fn direct_kid_store_writes_back_through_the_parent_array() {
    let mut pdf = empty_pdf();
    let mut leaf = Dictionary::new();
    leaf.insert("Names", Object::Array(Vec::new()));
    let mut root = Dictionary::new();
    root.insert(
        "Kids",
        Object::Array(vec![Object::Dictionary(leaf)]),
    );
    let mut tree = NNTree::<NameKey>::new(Object::Dictionary(root), false);
    let kid = NodeHandle::root().direct_kid(0);

    let mut changed = tree.load_node(&mut pdf, &kid).unwrap();
    changed.insert(
        "Names",
        Object::Array(vec![
            Object::String(b"a".to_vec()),
            Object::Integer(1),
        ]),
    );
    tree.store_node(&mut pdf, &kid, changed).unwrap();

    let Object::Dictionary(root) = tree.root() else {
        panic!("root must remain direct");
    };
    let Object::Array(kids) = root.get("Kids").unwrap() else {
        panic!("root /Kids must remain an array");
    };
    let Object::Dictionary(leaf) = &kids[0] else {
        panic!("kid must remain direct");
    };
    assert_eq!(
        leaf.get("Names"),
        Some(&Object::Array(vec![
            Object::String(b"a".to_vec()),
            Object::Integer(1),
        ]))
    );
}

#[test]
fn indirect_node_store_updates_the_terminal_holder_target() {
    let mut pdf = empty_pdf();
    let holder = ObjectRef::new(20, 0);
    let terminal = ObjectRef::new(21, 0);
    pdf.set_object(holder, Object::Reference(terminal));
    pdf.set_object(terminal, Object::Dictionary(Dictionary::new()));
    let mut tree = NNTree::<NameKey>::new(Object::Reference(holder), true);

    let node = tree.root_handle(&mut pdf).unwrap();
    let mut changed = tree.load_node(&mut pdf, &node).unwrap();
    changed.insert("Names", Object::Array(Vec::new()));
    tree.store_node(&mut pdf, &node, changed.clone()).unwrap();

    assert_eq!(pdf.resolve(terminal).unwrap(), Object::Dictionary(changed));
    assert_eq!(tree.root(), &Object::Reference(holder));
}
```

Run:

```sh
cargo test -p flpdf nntree::tests::direct_kid_store_writes_back_through_the_parent_array
cargo test -p flpdf nntree::tests::indirect_node_store_updates_the_terminal_holder_target
```

Expected: FAIL because `NNTree` and `NodeHandle` are undefined.

- [ ] **Step 5: Implement root/direct/indirect node storage**

Use these exact representations:

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
enum NodeAnchor {
    Root,
    Indirect(ObjectRef),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NodeHandle {
    anchor: NodeAnchor,
    direct_kids: Vec<usize>,
}

impl NodeHandle {
    fn root() -> Self {
        Self {
            anchor: NodeAnchor::Root,
            direct_kids: Vec::new(),
        }
    }

    fn indirect(object_ref: ObjectRef) -> Self {
        Self {
            anchor: NodeAnchor::Indirect(object_ref),
            direct_kids: Vec::new(),
        }
    }

    fn direct_kid(&self, kid_index: usize) -> Self {
        let mut direct_kids = self.direct_kids.clone();
        direct_kids.push(kid_index);
        Self {
            anchor: self.anchor.clone(),
            direct_kids,
        }
    }

    fn diagnostic_ref(&self) -> Option<ObjectRef> {
        match self.anchor {
            NodeAnchor::Indirect(object_ref) => Some(object_ref),
            NodeAnchor::Root => None,
        }
    }
}

pub(crate) struct NNTree<K: TreeKey> {
    root: Object,
    auto_repair: bool,
    split_threshold: usize,
    marker: PhantomData<K>,
}

impl<K: TreeKey> NNTree<K> {
    pub(crate) fn new(root: Object, auto_repair: bool) -> Self {
        Self {
            root,
            auto_repair,
            split_threshold: DEFAULT_SPLIT_THRESHOLD,
            marker: PhantomData,
        }
    }

    pub(crate) fn root(&self) -> &Object {
        &self.root
    }

    pub(crate) fn into_root(self) -> Object {
        self.root
    }
}
```

Implement `root_handle` by resolving a reference chain to its terminal
reference, or returning `NodeHandle::root()` for a direct dictionary.

Implement `load_node` by cloning the anchor dictionary, then following every
index in `direct_kids` through a direct dictionary in `/Kids`.

Implement `store_node` iteratively:

1. load every dictionary from the anchor through the direct path;
2. replace the final dictionary;
3. walk the saved dictionaries backward, replacing the selected `/Kids`
   element with the updated direct child;
4. write the rebuilt anchor to `self.root` or `Pdf::set_object`.

Use `Error::parse(0, structural_diagnostic(...))` for a non-dictionary,
missing `/Kids`, non-array `/Kids`, or out-of-range direct path.

Implement deterministic allocation:

```rust
fn make_indirect<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: Object,
) -> Result<ObjectRef> {
    let next = pdf
        .object_refs()
        .into_iter()
        .map(|object_ref| object_ref.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    let object_ref = ObjectRef::new(next, 0);
    pdf.set_object(object_ref, value);
    Ok(object_ref)
}
```

Run all `nntree::tests`. Expected: PASS.

- [ ] **Step 6: Commit the storage foundation**

```sh
git add crates/flpdf/src/lib.rs crates/flpdf/src/nntree.rs
git commit -m "refactor(nntree): add shared key and node storage"
```

---

### Task 2: Bidirectional cursor traversal

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`
- Test: `crates/flpdf/src/nntree.rs`

**Interfaces:**
- Consumes: `NNTree<K>`, `NodeHandle`, `load_node`, `store_node`, and `make_indirect`.
- Produces:

```rust
pub(crate) struct NNTreeCursor<K: TreeKey> {
    path: Vec<PathElement>,
    leaf: Option<NodeHandle>,
    item_number: Option<usize>,
    current: Option<(K::Key, Object)>,
    marker: PhantomData<K>,
}

impl<K: TreeKey> NNTree<K> {
    pub(crate) fn begin<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NNTreeCursor<K>>;
    pub(crate) fn end(&self) -> NNTreeCursor<K>;
    pub(crate) fn last<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<NNTreeCursor<K>>;
    pub(crate) fn next<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>, cursor: &mut NNTreeCursor<K>) -> Result<()>;
    pub(crate) fn previous<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>, cursor: &mut NNTreeCursor<K>) -> Result<()>;
}
```

- [ ] **Step 1: Add traversal RED tests**

Build a two-level tree with two indirect leaves:

```rust
fn two_leaf_name_tree(pdf: &mut TestPdf) -> Object {
    let left_ref = ObjectRef::new(10, 0);
    let right_ref = ObjectRef::new(11, 0);
    pdf.set_object(
        left_ref,
        name_leaf(&[(b"a", 1), (b"b", 2)], Some((b"a", b"b"))),
    );
    pdf.set_object(
        right_ref,
        name_leaf(&[(b"c", 3), (b"d", 4)], Some((b"c", b"d"))),
    );
    let mut root = Dictionary::new();
    root.insert(
        "Kids",
        Object::Array(vec![
            Object::Reference(left_ref),
            Object::Reference(right_ref),
        ]),
    );
    Object::Dictionary(root)
}
```

Add tests:

```rust
#[test]
fn begin_next_previous_last_and_end_match_qpdf_cursor_rules() {
    let mut pdf = empty_pdf();
    let root = two_leaf_name_tree(&mut pdf);
    let mut tree = NNTree::<NameKey>::new(root, true);

    let mut cursor = tree.begin(&mut pdf).unwrap();
    assert_eq!(
        cursor.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"a".as_slice(), &Object::Integer(1)))
    );
    tree.next(&mut pdf, &mut cursor).unwrap();
    assert_eq!(
        cursor.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"b".as_slice(), &Object::Integer(2)))
    );
    tree.next(&mut pdf, &mut cursor).unwrap();
    assert_eq!(
        cursor.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"c".as_slice(), &Object::Integer(3)))
    );

    let mut end = tree.end();
    assert!(!end.valid());
    tree.next(&mut pdf, &mut end).unwrap();
    assert_eq!(
        end.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"a".as_slice(), &Object::Integer(1)))
    );

    let mut end = tree.end();
    tree.previous(&mut pdf, &mut end).unwrap();
    assert_eq!(
        end.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"d".as_slice(), &Object::Integer(4)))
    );

    let mut last = tree.last(&mut pdf).unwrap();
    assert_eq!(
        last.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"d".as_slice(), &Object::Integer(4)))
    );
    tree.previous(&mut pdf, &mut last).unwrap();
    assert_eq!(
        last.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"c".as_slice(), &Object::Integer(3)))
    );
}

#[test]
fn direct_kid_is_indirectized_only_when_auto_repair_is_enabled() {
    let mut repaired_pdf = empty_pdf();
    let root = root_with_one_direct_leaf();
    let mut repaired = NNTree::<NameKey>::new(root, true);
    let cursor = repaired.begin(&mut repaired_pdf).unwrap();
    assert!(cursor.valid());
    assert!(matches!(
        repaired.root(),
        Object::Dictionary(root)
            if matches!(
                root.get("Kids"),
                Some(Object::Array(kids))
                    if matches!(kids.first(), Some(Object::Reference(_)))
            )
    ));
    assert!(repaired_pdf
        .repair_diagnostics()
        .entries()
        .iter()
        .any(|entry| entry
            .message
            .contains("converting kid number 0 to an indirect object")));

    let mut strict_pdf = empty_pdf();
    let root = root_with_one_direct_leaf();
    let mut strict = NNTree::<NameKey>::new(root, false);
    let cursor = strict.begin(&mut strict_pdf).unwrap();
    assert!(cursor.valid());
    assert!(matches!(
        strict.root(),
        Object::Dictionary(root)
            if matches!(
                root.get("Kids"),
                Some(Object::Array(kids))
                    if matches!(kids.first(), Some(Object::Dictionary(_)))
            )
    ));
}
```

Run each new test. Expected: FAIL because cursor APIs do not exist.

- [ ] **Step 2: Implement cursor state and current-item update**

Add:

```rust
#[derive(Clone, Debug)]
struct PathElement {
    node: NodeHandle,
    kid_number: usize,
}

pub(crate) struct NNTreeCursor<K: TreeKey> {
    path: Vec<PathElement>,
    leaf: Option<NodeHandle>,
    item_number: Option<usize>,
    current: Option<(K::Key, Object)>,
    marker: PhantomData<K>,
}

impl<K: TreeKey> NNTreeCursor<K> {
    fn empty() -> Self {
        Self {
            path: Vec::new(),
            leaf: None,
            item_number: None,
            current: None,
            marker: PhantomData,
        }
    }

    pub(crate) fn valid(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) fn current(&self) -> Option<(&K::Key, &Object)> {
        self.current
            .as_ref()
            .map(|(key, value)| (key, value))
    }

    fn cloned_current(&self) -> Option<(K::Key, Object)> {
        self.current.clone()
    }
}
```

`update_current` must:

- clear `current` when no leaf or item index is selected;
- require an items array with at least key and value at the selected even
  index;
- parse the key through `K::from_object`;
- when `allow_invalid` is true, record the qpdf warning and leave the cursor
  invalid for bad keys or short arrays;
- when false, return `Error::Parse` with `update ivalue: items array is too
  short` or `item at index N is not the right type`.

- [ ] **Step 3: Port qpdf deepen and cross-leaf movement**

Translate `NNTreeIterator::deepen`, `getNextKid`, and `increment` from
`NNTree.cc:80-156,585-664`.

Use these rules:

```rust
fn descend<R: Read + Seek>(
    &mut self,
    pdf: &mut Pdf<R>,
    cursor: &mut NNTreeCursor<K>,
    start: NodeHandle,
    first: bool,
    allow_empty: bool,
) -> Result<bool>
```

- Prefer a non-empty items array over `/Kids`.
- For `/Kids`, select index 0 when `first`, otherwise the last index.
- Add the parent `PathElement` before descending.
- Resolve an indirect kid to its terminal reference.
- With `auto_repair`, convert a direct kid to an indirect object, replace the
  exact parent slot, and warn.
- Without `auto_repair`, warn but retain a direct-kid `NodeHandle`.
- Permit an empty items array only when `allow_empty`.
- On a bad node, restore the cursor's original path and return `false`.

`next` and `previous` must:

- turn `end` into begin/last respectively;
- move by two within a leaf;
- cross into the next/previous sibling and descend to its edge leaf;
- become invalid after moving beyond the last/first item.

Run both traversal tests and all prior tests. Expected: PASS.

- [ ] **Step 4: Add malformed traversal and cycle tests**

Add literal tests for:

- non-dictionary root;
- node with neither non-empty items nor `/Kids`;
- empty items with `allow_empty`;
- `/Kids` containing an indirect cycle;
- odd-length items array;
- wrong key type.

Each test must assert the exact result and diagnostic substring observed from
qpdf 11.9.0. Use `libtests/nntree.cc` and a live qpdf fixture generated under
`/tmp` when source alone does not reveal warning order.

Run all `nntree::tests`. Expected: at least one new test FAIL before adding
the missing diagnostic/cycle branch, then PASS after the branch is ported.

- [ ] **Step 5: Commit bidirectional traversal**

```sh
git add crates/flpdf/src/nntree.rs
git commit -m "feat(nntree): port bidirectional cursor traversal"
```

---

### Task 3: Targeted find and binary search

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`
- Test: `crates/flpdf/src/nntree.rs`

**Interfaces:**
- Consumes: traversal cursor and node storage.
- Produces:

```rust
impl<K: TreeKey> NNTree<K> {
    pub(crate) fn find<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
        return_previous_if_missing: bool,
    ) -> Result<NNTreeCursor<K>>;
}
```

- Produces `find_internal`, `within_limits`, and the exact qpdf power-of-two
  `binary_search` used later by insert and remove.

- [ ] **Step 1: Add exact/previous/missing RED tests**

```rust
#[test]
fn find_returns_exact_previous_or_end_like_qpdf() {
    let mut pdf = empty_pdf();
    let mut tree = NNTree::<NumberKey>::new(two_leaf_number_tree(&mut pdf), true);

    assert_eq!(
        tree.find(&mut pdf, &20, false).unwrap().current(),
        Some((&20, &Object::String(b"twenty".to_vec())))
    );
    assert!(!tree.find(&mut pdf, &25, false).unwrap().valid());
    assert_eq!(
        tree.find(&mut pdf, &25, true).unwrap().current(),
        Some((&20, &Object::String(b"twenty".to_vec())))
    );
    assert!(!tree.find(&mut pdf, &-1, true).unwrap().valid());
}
```

Add a name-key version whose lookup crosses a `/Limits` boundary.

Run the tests. Expected: FAIL because `find` is absent.

- [ ] **Step 2: Port qpdf's binary search literally**

Implement a generic index helper with the same max/step/check sequence as
`NNTreeImpl::binarySearch`:

```rust
fn binary_search<F>(
    count: usize,
    return_previous_if_missing: bool,
    mut compare: F,
) -> Result<Option<usize>>
where
    F: FnMut(usize) -> Result<Ordering>,
{
    let mut max_index = 1usize;
    while max_index < count {
        max_index <<= 1;
    }
    let mut step = max_index / 2;
    let mut checks = max_index;
    let mut index = step;
    let mut found_index = None;
    let mut found = false;
    let mut found_less_or_equal = false;
    let mut status = Ordering::Equal;

    while !found && checks > 0 {
        status = if index < count {
            let status = compare(index)?;
            if status != Ordering::Less {
                found_less_or_equal = true;
                found_index = Some(index);
            }
            status
        } else {
            Ordering::Less
        };
        if status == Ordering::Equal {
            found = true;
        } else {
            checks >>= 1;
            if checks > 0 {
                step >>= 1;
                if step == 0 {
                    step = 1;
                }
                if status == Ordering::Less {
                    index = index.saturating_sub(step);
                } else {
                    index = index.saturating_add(step);
                }
            }
        }
    }

    Ok(if found || (found_less_or_equal && return_previous_if_missing) {
        found_index
    } else {
        None
    })
}
```

Before accepting this translation, compare its table of results for counts
0..40 and every insertion point against a tiny qpdf-oracle harness under
`/tmp`. If the literal Rust state differs, fix the Rust translation rather
than adjusting expected results.

- [ ] **Step 3: Implement `/Limits`, item comparison, and `find_internal`**

Port `NNTreeImpl::withinLimits`, `compareKeyItem`, `compareKeyKid`, and
`findInternal` from `NNTree.cc:704-901`.

Required diagnostics:

```text
node is missing /Limits
item at index N is not the right type
invalid kid at index N
unexpected -1 from binary search of kids; limits may by wrong
loop detected in find
bad node during find
```

`find` initially delegates directly to `find_internal`; repair wrapping is
added in Task 5.

Run exact/previous/missing tests. Expected: PASS.

- [ ] **Step 4: Add structural find RED tests and complete diagnostics**

Add one test per diagnostic above. Each test uses a hand-built literal object
graph and asserts the qpdf node-number prefix:

```rust
let error = tree.find(&mut pdf, &b"z".to_vec(), false).unwrap_err();
assert_eq!(
    error.to_string(),
    "parse error at byte 0: Name/Number tree node (object 9): node is missing /Limits"
);
```

Run each test before its branch exists, confirm the expected failure, then add
only that branch. Run all `nntree::tests`. Expected: PASS.

- [ ] **Step 5: Commit lookup**

```sh
git add crates/flpdf/src/nntree.rs
git commit -m "feat(nntree): port targeted lookup"
```

---

### Task 4: Insert, remove, limits, and recursive splitting

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`
- Test: `crates/flpdf/src/nntree.rs`

**Interfaces:**
- Consumes: cursor traversal and `find`.
- Produces:

```rust
impl<K: TreeKey> NNTree<K> {
    pub(crate) fn set_split_threshold(&mut self, threshold: usize);
    pub(crate) fn insert<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: K::Key,
        value: Object,
    ) -> Result<NNTreeCursor<K>>;
    pub(crate) fn insert_after<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
        key: K::Key,
        value: Object,
    ) -> Result<()>;
    pub(crate) fn remove<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        key: &K::Key,
    ) -> Result<Option<Object>>;
    pub(crate) fn remove_at<R: Read + Seek>(
        &mut self,
        pdf: &mut Pdf<R>,
        cursor: &mut NNTreeCursor<K>,
    ) -> Result<Option<Object>>;
}
```

- [ ] **Step 1: Add insertion/replacement/removal RED tests**

```rust
#[test]
fn insert_orders_keys_replaces_duplicates_and_remove_returns_value() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);

    tree.insert(&mut pdf, 20, Object::String(b"old".to_vec())).unwrap();
    tree.insert(&mut pdf, 10, Object::String(b"ten".to_vec())).unwrap();
    tree.insert(&mut pdf, 30, Object::String(b"thirty".to_vec())).unwrap();
    tree.insert(&mut pdf, 20, Object::String(b"new".to_vec())).unwrap();

    assert_eq!(
        collect_number_entries(&mut tree, &mut pdf),
        vec![
            (10, Object::String(b"ten".to_vec())),
            (20, Object::String(b"new".to_vec())),
            (30, Object::String(b"thirty".to_vec())),
        ]
    );
    assert_eq!(
        tree.remove(&mut pdf, &20).unwrap(),
        Some(Object::String(b"new".to_vec()))
    );
    assert_eq!(tree.remove(&mut pdf, &20).unwrap(), None);
}
```

Add `insert_after` and `remove_at` cursor-position assertions from
`libtests/nntree.cc`.

Run. Expected: FAIL because mutation APIs are absent.

- [ ] **Step 2: Implement item mutation and limit reset**

Port `NNTreeIterator::resetLimits`, `insertAfter`, and `remove` from
`NNTree.cc:157-223,389-518`.

`reset_limits` must:

- remove `/Limits` from the root;
- for a non-root node, take the first and last keys from its item array or
  first/last child `/Limits`;
- update every ancestor whose first or last child changed;
- use key objects, not formatted Rust keys, in `/Limits`;
- store direct nodes back through `NodeHandle`.

After every mutation, call `update_current` and preserve qpdf's cursor advance.

Run insertion/removal tests. Expected: PASS before split-threshold tests.

- [ ] **Step 3: Add forced leaf/root/internal split RED tests**

Set the threshold to 4 so small literal trees force all split branches:

```rust
#[test]
fn split_promotes_root_and_recursively_splits_parent_in_qpdf_order() {
    let mut pdf = empty_pdf();
    let mut root = Dictionary::new();
    root.insert("Nums", Object::Array(Vec::new()));
    let mut tree = NNTree::<NumberKey>::new(Object::Dictionary(root), true);
    tree.set_split_threshold(4);

    for key in 0..13 {
        tree.insert(&mut pdf, key, Object::Integer(key)).unwrap();
    }

    assert_eq!(
        number_tree_shape(&mut pdf, tree.root()),
        concat!(
            "K(",
            "K(K(L[0],L[1]),K(L[2],L[3])),",
            "K(K(L[4],L[5]),K(L[6],L[7]),K(L[8],L[9],L[10],L[11,12]))",
            ")"
        )
    );
    let Object::Dictionary(root) = tree.root() else {
        panic!("root must be a dictionary");
    };
    assert!(!root.contains_key("Limits"));
}
```

Confirm the literal shape against a qpdf 11.9.0 harness under `/tmp`. Do not
derive the expected string from the flpdf splitting code. Add a recursive
limits assertion beside `number_tree_shape`: every non-root dictionary must
have `/Limits` equal to its first and last descendant keys.

Run. Expected: FAIL because split is absent.

- [ ] **Step 4: Port recursive split**

Translate `NNTreeIterator::split` from `NNTree.cc:224-377` without replacing it
with collect/rebuild.

Required operations:

1. return when the items/kids array length is within threshold;
2. when splitting the root, create an indirect first node, move root contents
   into it, and replace the root with `/Kids [first]`;
3. split the selected items/kids array at `((n / 2) & !1)`;
4. reset first and second node limits;
5. insert the second indirect node immediately after the first;
6. move the cursor to the correct half;
7. recursively split the parent when it now exceeds the threshold.

For `/Kids`, threshold counts array elements. For `/Names` and `/Nums`,
threshold counts key/value array elements exactly as qpdf does.

Run all mutation and split tests. Expected: PASS.

- [ ] **Step 5: Add default-threshold byte-shape tests**

Insert 33 name pairs and assert qpdf's 16/17 split, object allocation order,
root `/Limits` omission, and leaf `/Limits`. Add the equivalent number-tree
case. Compare the resulting raw objects to a live qpdf 11.9.0 dump.

Run all `nntree::tests`. Expected: PASS.

- [ ] **Step 6: Commit mutation and splitting**

```sh
git add crates/flpdf/src/nntree.rs
git commit -m "feat(nntree): port mutation and recursive splitting"
```

---

### Task 5: Auto-repair and malformed-tree parity

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`
- Test: `crates/flpdf/src/nntree.rs`

**Interfaces:**
- Consumes: traversal, find, insert, and split.
- Produces `repair` and the final `find` retry contract.

- [ ] **Step 1: Add repair-enabled/disabled RED tests**

```rust
#[test]
fn find_repairs_once_and_retries_when_auto_repair_is_enabled() {
    let mut pdf = empty_pdf();
    let root = malformed_name_tree_with_missing_limits_and_valid_pairs(&mut pdf);
    let mut tree = NNTree::<NameKey>::new(root, true);

    let found = tree.find(&mut pdf, &b"beta".to_vec(), false).unwrap();
    assert_eq!(
        found.current().map(|(key, value)| (key.as_slice(), value)),
        Some((b"beta".as_slice(), &Object::Integer(2)))
    );
    let warnings = warning_messages(&pdf);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("attempting to repair after error:"));
    assert!(warnings[0].contains("node is missing /Limits"));
    assert_eq!(
        collect_name_entries(&mut tree, &mut pdf),
        vec![
            (b"alpha".to_vec(), Object::Integer(1)),
            (b"beta".to_vec(), Object::Integer(2)),
        ]
    );
}

#[test]
fn find_does_not_mutate_when_auto_repair_is_disabled() {
    let mut pdf = empty_pdf();
    let root = malformed_name_tree_with_missing_limits_and_valid_pairs(&mut pdf);
    let original = root.clone();
    let mut tree = NNTree::<NameKey>::new(root, false);

    let error = tree
        .find(&mut pdf, &b"beta".to_vec(), false)
        .unwrap_err();
    assert!(error.to_string().contains("node is missing /Limits"));
    assert_eq!(tree.root(), &original);
}
```

Run. Expected: FAIL because `find` does not repair.

- [ ] **Step 2: Implement qpdf repair and one retry**

Port `NNTreeImpl::repair` and `find` from `NNTree.cc:807-835`.

Implement `repair` as:

```rust
fn repair<R: Read + Seek>(&mut self, pdf: &mut Pdf<R>) -> Result<()> {
    let mut replacement_root = Dictionary::new();
    replacement_root.insert(K::ITEMS_KEY, Object::Array(Vec::new()));
    let mut replacement =
        NNTree::<K>::new(Object::Dictionary(replacement_root), false);
    replacement.set_split_threshold(self.split_threshold);

    let mut cursor = self.begin(pdf)?;
    while let Some((key, value)) = cursor.cloned_current() {
        replacement.insert(pdf, key, value)?;
        self.next(pdf, &mut cursor)?;
    }

    self.replace_root_contents(replacement.into_root(), pdf)
}
```

`replace_root_contents` replaces only `/Kids` and the codec items key, matching
qpdf; it preserves unrelated root dictionary keys and removes the obsolete
alternative tree-shape key.

Wrap `find_internal`:

```rust
pub(crate) fn find<R: Read + Seek>(
    &mut self,
    pdf: &mut Pdf<R>,
    key: &K::Key,
    return_previous_if_missing: bool,
) -> Result<NNTreeCursor<K>> {
    match self.find_internal(pdf, key, return_previous_if_missing) {
        Ok(cursor) => Ok(cursor),
        Err(error) if self.auto_repair => {
            self.warn(
                pdf,
                self.root_handle(pdf).ok().as_ref(),
                &format!("attempting to repair after error: {error}"),
            );
            self.repair(pdf)?;
            self.find_internal(pdf, key, return_previous_if_missing)
        }
        Err(error) => Err(error),
    }
}
```

Avoid a second mutable borrow of `pdf` in the actual implementation by
computing the root handle before calling `warn`.

Run repair-enabled/disabled tests. Expected: PASS.

- [ ] **Step 3: Add malformed-tree repair matrix RED tests**

Port the Layer-1-relevant cases from existing
`outline_document_helper_tests.rs` into internal engine tests:

- wrong key type;
- short items array;
- invalid kid;
- missing `/Limits`;
- bad node;
- indirect loop;
- empty root;
- zero surviving entries;
- more than one repaired leaf;
- repaired parent split order;
- direct root and terminal indirect root.

For each case, assert:

- exact ordered diagnostics;
- resulting root shape;
- surviving sorted pairs;
- allocation order; and
- a successful retried find when qpdf succeeds.

Run each test before adding any missing repair warning/branch. Expected: FAIL
for the intended branch, then PASS after the minimal port.

- [ ] **Step 4: Verify repair does not swallow fatal structural errors**

Add and pass tests matching qpdf for:

- a short first pair that remains fatal after the repair warning;
- object-number exhaustion while indirectizing a direct kid;
- resolution failure from a broken reference;
- repair retry failing a second time.

Errors must propagate; do not add a retry loop or best-effort fallback.

- [ ] **Step 5: Commit repair**

```sh
git add crates/flpdf/src/nntree.rs
git commit -m "feat(nntree): port structural auto-repair"
```

---

### Task 6: Layer-1 documentation, verification, and coverage

**Files:**
- Modify: `crates/flpdf/src/nntree.rs`
- Test: all workspace tests

**Interfaces:**
- Consumes: completed crate-private engine.
- Produces: independently reviewable Layer-1 branch ready to become the base
  of `flpdf-qxba.8.2`.

- [ ] **Step 1: Audit qpdf source correspondence**

At the top of `nntree.rs`, cite the exact qpdf regions owned by this layer:

```rust
//! Mirrors qpdf 11.9.0 `libqpdf/NNTree.cc`.
//! Internal API corresponds to `libqpdf/qpdf/NNTree.hh`.
//! Typed public wrappers for `QPDFNameTreeObjectHelper` and
//! `QPDFNumberTreeObjectHelper` are added by `flpdf-qxba.8.2`.
```

Use:

```sh
rg -n '^NNTreeIterator::|^NNTreeImpl::' \
  "$(scripts/fetch-qpdf-source.sh --print-path)/libqpdf/NNTree.cc"
```

Check every listed method against either production code or an explicitly
documented Layer-2 wrapper-only method. No `NNTreeImpl` algorithm may be
deferred.

- [ ] **Step 2: Run formatting and focused tests**

```sh
cargo fmt
cargo fmt --all -- --check
cargo test -p flpdf nntree::tests::
```

Expected: all pass with no warnings.

- [ ] **Step 3: Run workspace quality gates**

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p flpdf
cargo test
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: all pass.

- [ ] **Step 4: Commit any verification-only fixes**

Only if formatting, clippy, rustdoc, or tests required source changes:

```sh
git add crates/flpdf/src/nntree.rs crates/flpdf/src/lib.rs
git commit -m "test(nntree): complete engine parity gates"
```

Do not create an empty commit.
Do not modify `docs/qpdf-correspondence.md` in Layer 1; Layer 3 owns the final
consumer-consolidation status and correspondence update.

- [ ] **Step 5: Measure committed-head patch coverage**

Ensure the tree is clean, then run:

```sh
cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail \
  --lcov --output-path target/flpdf-qxba-8-1.lcov
scripts/patch-coverage.sh \
  --base origin/main \
  --lcov target/flpdf-qxba-8-1.lcov
```

Expected:

```text
Patch coverage: 100.00%
```

If an executable changed line is uncovered, add a behavior test that would
fail when that line's branch is wrong. Do not add a reason-less
`cov:ignore`.

- [ ] **Step 6: Verify final scope**

```sh
git status --short --branch
git diff --stat origin/main..HEAD
git log --oneline origin/main..HEAD
```

Expected:

- clean worktree;
- changes limited to the design/plan, `lib.rs`, and `nntree.rs` except for an
  evidence-backed correspondence-doc line update;
- no edits to current consumers;
- no commits from `flpdf-qxba.6`.

- [ ] **Step 7: Update tracker and persist**

Record exact gate results in `flpdf-qxba.8.1`, close it, and persist Beads:

```sh
bd update flpdf-qxba.8.1 --append-notes \
  "Layer 1 gates: fmt=pass; workspace clippy=pass; flpdf tests=pass; workspace tests=pass; strict rustdoc=pass; patch coverage=100.00% against origin/main."
bd close flpdf-qxba.8.1 --reason "Shared qpdf NNTree engine implemented and verified"
bd dolt pull
bd dolt push
```

- [ ] **Step 8: Push the bottom branch**

```sh
git push -u origin feature/flpdf-qxba-8-1-engine
```

Do not open the PR until the user requests PR creation or the branch-finishing
decision explicitly selects it.
