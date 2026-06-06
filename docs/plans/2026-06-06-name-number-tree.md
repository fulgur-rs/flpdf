# Generic Name/Number Tree Iteration Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Extract a single source of truth for PDF name-tree / number-tree walking
(`name_number_tree` module) and refactor the existing `embedded_files` readers +
rebuild and `json_inspect::walk_pagelabels` onto it, with byte-identical output.

**Architecture:** Name trees (`/Names` leaf, `Object::String` keys) and number
trees (`/Nums` leaf, `Object::Integer` keys) share identical structure (`/Kids`
intermediate nodes, optional `/Limits`, DFS order, depth + cycle guards). One
internal generic `walk_tree` is wrapped by two public readers parameterized by a
**decode hook** `FnMut(&mut Pdf, Object) -> Result<Option<V>>` (generic over the
value type — covers raw / ref-only / resolved-dict views). A pure `build_name_tree`
reproduces the `embedded_files` writer's exact node layout. Consumer-specific
catalog/`/AF`/GC/prune logic is NOT moved into the generic.

**Tech Stack:** Rust, `flpdf` crate. `cargo test -p flpdf`, `cargo clippy`.

**Issue:** flpdf-9hc.18.4. Design lives in the beads issue `design` field.

---

## Key references (read before starting)

- `crates/flpdf/src/embedded_files.rs` — name-tree readers
  (`collect_name_tree`/`collect_name_tree_dict`/`collect_leaf_pairs` at lines
  ~437-515; raw variants ~564-625), writer `rebuild_embedded_files_tree`
  (~703-827), `build_leaf_dict` (~834-856), `LEAF_MAX` const (line 341),
  `DEFAULT_MAX_EMBEDDED_FILES_DEPTH` (line 347).
- `crates/flpdf/src/json_inspect.rs` — `walk_pagelabels` (~929-987),
  `build_pagelabels_section` (~999-1056). Uses `ConvertError` with
  `.map_err(ConvertError::from)`; `From<crate::Error> for ConvertError` exists.
- `crates/flpdf/src/object.rs` — `Object::as_array` (164), `as_dict` (132),
  `as_ref_id` (220), `as_integer` (196). `ObjectRef::new(num, gen)`.
- `crates/flpdf/src/lib.rs` — module decls (~40-85), re-exports (~90-147).
  `embedded_files` re-export incl. `LEAF_MAX` at lines 103-107.
- Review rules: `.claude/rules/pdf-rust-review-patterns.md` — esp. #1 (no
  needless `.clone()`), #2 (resolve indirect refs), #4 (depth + cycle guards).

## Behavior-preservation contract (the verification bar)

- `embedded_files` has a large regression suite tied to roborev #947-#951. The
  pass/fail bar is **the whole `cargo test -p flpdf` suite green**, not just new
  tests. `build_name_tree` MUST emit byte-identical trees (same `LEAF_MAX=32`,
  `div_ceil` chunking, `/Limits` on every node, single-leaf-no-`/Kids` vs
  root+`/Kids`, **alloc call order = leaves-then-root**).
- Depth semantics are standardized on `depth >= max_depth -> Err(Error::Unsupported)`
  (matches `embedded_files`). This changes `walk_pagelabels` from silent
  truncation to an error on pathological depth > 100; no test asserts the old
  truncation and real trees are depth << 100, so no observable change on
  well-formed input. Call this out in the Task 6 commit message.

---

## Task 1: Module skeleton + `read_name_tree`

**Files:**
- Create: `crates/flpdf/src/name_number_tree.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `pub mod name_number_tree;` near line 60,
  alphabetical — after `object_copy`, before `outline`)

**Step 1: Add the module declaration**

In `lib.rs`, add (keeping alphabetical order with neighbours):

```rust
pub mod name_number_tree;
```

**Step 2: Write the module with the generic core + `read_name_tree` + failing tests**

Create `crates/flpdf/src/name_number_tree.rs`:

```rust
//! Generic name-tree / number-tree iteration (ISO 32000-1 §7.9.6 / §7.9.7).
//!
//! Name trees (`/Names` leaf, string keys) and number trees (`/Nums` leaf,
//! integer keys) share the same shape: `/Kids` intermediate nodes, an optional
//! `/Limits [least greatest]` array, depth-first key-ascending order, and the
//! need for depth + cycle guards against hostile or cyclic `/Kids` chains.
//!
//! [`read_name_tree`] / [`read_number_tree`] enumerate a tree, decoding each
//! value via a caller-supplied hook (generic over the value type, so the same
//! walker serves verbatim-`Object`, reference-only, and resolved-`Dictionary`
//! views). [`build_name_tree`] rebuilds a name tree from sorted entries.
//!
//! This module owns only structural concerns (parse + build). Catalog wiring,
//! `/AF` upkeep, GC, and prune-during-walk stay in the consumer.

use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

/// Default `/Kids` descent depth limit (cyclic / maliciously deep guard).
pub const DEFAULT_MAX_TREE_DEPTH: usize = 100;

/// Max entries in a single leaf before [`build_name_tree`] splits into a
/// `/Kids` root (mirrors qpdf's aggressive rebuild threshold).
pub const LEAF_MAX: usize = 32;

/// Enumerate a **name** tree rooted at `root` (a `/Kids` root node reference,
/// or an inline node dictionary), decoding each value via `decode`.
///
/// Entries are returned in depth-first order (the spec mandates keys be sorted).
/// `decode` returning `Ok(None)` skips that entry; non-string keys and the
/// trailing orphan of an odd-length leaf array are dropped silently.
///
/// # Errors
/// Propagates [`Pdf::resolve`] errors and returns [`crate::Error::Unsupported`]
/// if a `/Kids` chain reaches `max_depth`.
pub fn read_name_tree<R, V, F>(
    pdf: &mut Pdf<R>,
    root: Object,
    mut decode: F,
    max_depth: usize,
) -> Result<Vec<(Vec<u8>, V)>>
where
    R: Read + Seek,
    F: FnMut(&mut Pdf<R>, Object) -> Result<Option<V>>,
{
    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    walk_tree(
        pdf,
        root,
        "Names",
        &|o| match o {
            Object::String(b) => Some(b),
            _ => None,
        },
        &mut decode,
        &mut out,
        &mut visited,
        0,
        max_depth,
    )?;
    Ok(out)
}

/// Internal generic walker shared by name + number readers.
///
/// `node` is a `Reference` (resolved + cycle-tracked here) or a `Dictionary`.
/// `leaf_key` is `"Names"` or `"Nums"`; `parse_key` converts a leaf key object
/// to `K` (or `None` to skip the pair).
#[allow(clippy::too_many_arguments)]
fn walk_tree<R, K, V, FK, FV>(
    pdf: &mut Pdf<R>,
    node: Object,
    leaf_key: &str,
    parse_key: &FK,
    decode: &mut FV,
    out: &mut Vec<(K, V)>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
    max_depth: usize,
) -> Result<()>
where
    R: Read + Seek,
    FK: Fn(Object) -> Option<K>,
    FV: FnMut(&mut Pdf<R>, Object) -> Result<Option<V>>,
{
    if depth >= max_depth {
        return Err(crate::Error::Unsupported(format!(
            "name_number_tree: /Kids depth limit {max_depth} exceeded"
        )));
    }

    // Resolve a reference node (cycle-tracked); inline dicts pass through.
    let dict: Dictionary = match node {
        Object::Dictionary(d) => d,
        Object::Reference(r) => {
            if !visited.insert(r) {
                return Ok(()); // cycle — skip
            }
            match pdf.resolve_borrowed(r)?.as_dict() {
                Some(d) => d.clone(),
                None => return Ok(()), // malformed node — skip
            }
        }
        _ => return Ok(()), // unexpected node type — skip
    };

    // Leaf takes priority over /Kids (spec leaf vs. intermediate).
    if let Some(arr) = dict.get(leaf_key).and_then(Object::as_array) {
        let pairs = arr.to_vec(); // own the leaf array, drop the dict borrow
        let mut it = pairs.into_iter();
        while let Some(key_obj) = it.next() {
            let Some(val_obj) = it.next() else {
                break; // odd-length array — drop orphan key
            };
            let Some(key) = parse_key(key_obj) else {
                continue; // non-matching key type — skip pair
            };
            if let Some(v) = decode(pdf, val_obj)? {
                out.push((key, v));
            }
        }
        return Ok(());
    }

    // Intermediate node.
    if let Some(kids) = dict.get("Kids").and_then(Object::as_array) {
        for kid in kids.to_vec() {
            walk_tree(
                pdf, kid, leaf_key, parse_key, decode, out, visited, depth + 1,
                max_depth,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_pdf() -> Pdf<std::io::Cursor<Vec<u8>>> {
        // Minimal valid PDF; the readers don't need a real catalog because we
        // pass nodes directly via set_object refs.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"%PDF-1.4\n");
        let off1 = bytes.len() as u64;
        bytes.extend_from_slice(b"1 0 obj\n<< /Type /Catalog >>\nendobj\n");
        let xref = bytes.len() as u64;
        bytes.extend_from_slice(
            format!(
                "xref\n0 2\n0000000000 65535 f \n{off1:010} 00000 n \ntrailer\n<< /Size 2 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        Pdf::open(std::io::Cursor::new(bytes)).expect("open")
    }

    #[test]
    fn read_name_tree_inline_leaf_ref_only() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"a".to_vec()),
                Object::Reference(ObjectRef::new(10, 0)),
                Object::String(b"b".to_vec()),
                Object::Reference(ObjectRef::new(11, 0)),
            ]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            |_, v| Ok(v.as_ref_id()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                (b"a".to_vec(), ObjectRef::new(10, 0)),
                (b"b".to_vec(), ObjectRef::new(11, 0)),
            ]
        );
    }

    #[test]
    fn read_name_tree_skips_when_decode_none() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"a".to_vec()),
                Object::Integer(5), // not a ref -> decode returns None -> skipped
                Object::String(b"b".to_vec()),
                Object::Reference(ObjectRef::new(11, 0)),
            ]),
        );
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            |_, v| Ok(v.as_ref_id()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"b".to_vec(), ObjectRef::new(11, 0))]);
    }

    #[test]
    fn read_name_tree_descends_kids_via_reference() {
        let mut pdf = empty_pdf();
        // Leaf object at ref 20.
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Names",
            Object::Array(vec![
                Object::String(b"k".to_vec()),
                Object::Reference(ObjectRef::new(99, 0)),
            ]),
        );
        let leaf_ref = ObjectRef::new(20, 0);
        pdf.set_object(leaf_ref, Object::Dictionary(leaf));
        // Root with /Kids -> [20 0 R].
        let mut root = Dictionary::new();
        root.insert("Kids", Object::Array(vec![Object::Reference(leaf_ref)]));
        let out = read_name_tree(
            &mut pdf,
            Object::Dictionary(root),
            |_, v| Ok(v.as_ref_id()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(b"k".to_vec(), ObjectRef::new(99, 0))]);
    }

    #[test]
    fn read_name_tree_cycle_terminates() {
        let mut pdf = empty_pdf();
        // Node 30 has /Kids -> [30 0 R] (self-cycle).
        let mut node = Dictionary::new();
        let node_ref = ObjectRef::new(30, 0);
        node.insert("Kids", Object::Array(vec![Object::Reference(node_ref)]));
        pdf.set_object(node_ref, Object::Dictionary(node));
        let out = read_name_tree(
            &mut pdf,
            Object::Reference(node_ref),
            |_, v| Ok(v.as_ref_id()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_name_tree_depth_limit_errors() {
        let mut pdf = empty_pdf();
        // Chain of /Kids deeper than the limit.
        // 40 -> 41 -> 42 ...; with max_depth=2 the third level errors.
        let r40 = ObjectRef::new(40, 0);
        let r41 = ObjectRef::new(41, 0);
        let r42 = ObjectRef::new(42, 0);
        for (this, next) in [(r40, r41), (r41, r42)] {
            let mut d = Dictionary::new();
            d.insert("Kids", Object::Array(vec![Object::Reference(next)]));
            pdf.set_object(this, Object::Dictionary(d));
        }
        let mut leaf = Dictionary::new();
        leaf.insert("Names", Object::Array(vec![]));
        pdf.set_object(r42, Object::Dictionary(leaf));
        let err = read_name_tree(
            &mut pdf,
            Object::Reference(r40),
            |_, v: Object| Ok(Some(v)),
            2,
        );
        assert!(matches!(err, Err(crate::Error::Unsupported(_))));
    }
}
```

**Step 3: Run the new tests (expect PASS — code is written alongside)**

Run: `cargo test -p flpdf --lib name_number_tree`
Expected: all `name_number_tree::tests::*` PASS.

**Step 4: Clippy clean on the new module**

Run: `cargo clippy -p flpdf --all-targets 2>&1 | grep name_number_tree`
Expected: no warnings.

**Step 5: Commit**

```bash
git add crates/flpdf/src/name_number_tree.rs crates/flpdf/src/lib.rs
git commit -m "feat(name_number_tree): generic name-tree reader with decode hook (flpdf-9hc.18.4)"
```

---

## Task 2: `read_number_tree`

**Files:**
- Modify: `crates/flpdf/src/name_number_tree.rs`

**Step 1: Add `read_number_tree` (above the `walk_tree` fn or after `read_name_tree`)**

```rust
/// Enumerate a **number** tree rooted at `root` (a `/Kids` root node reference,
/// or an inline node dictionary), decoding each value via `decode`.
///
/// Same semantics as [`read_name_tree`] but with `/Nums` leaves and integer
/// keys; non-integer keys are skipped.
///
/// # Errors
/// Propagates [`Pdf::resolve`] errors and returns [`crate::Error::Unsupported`]
/// if a `/Kids` chain reaches `max_depth`.
pub fn read_number_tree<R, V, F>(
    pdf: &mut Pdf<R>,
    root: Object,
    mut decode: F,
    max_depth: usize,
) -> Result<Vec<(i64, V)>>
where
    R: Read + Seek,
    F: FnMut(&mut Pdf<R>, Object) -> Result<Option<V>>,
{
    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    walk_tree(
        pdf,
        root,
        "Nums",
        &|o| match o {
            Object::Integer(n) => Some(n),
            _ => None,
        },
        &mut decode,
        &mut out,
        &mut visited,
        0,
        max_depth,
    )?;
    Ok(out)
}
```

**Step 2: Add tests in the `tests` module**

```rust
    #[test]
    fn read_number_tree_resolves_indirect_dict_value() {
        let mut pdf = empty_pdf();
        // Value at ref 50 is a label dict.
        let mut label = Dictionary::new();
        label.insert("S", Object::Name("D".into()));
        let label_ref = ObjectRef::new(50, 0);
        pdf.set_object(label_ref, Object::Dictionary(label));
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![
                Object::Integer(0),
                Object::Reference(label_ref), // indirect value -> resolve
                Object::Integer(5),
                Object::Dictionary({
                    let mut d = Dictionary::new();
                    d.insert("S", Object::Name("R".into()));
                    d
                }),
            ]),
        );
        let out: Vec<(i64, Dictionary)> = read_number_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            |pdf, v| match v {
                Object::Dictionary(d) => Ok(Some(d)),
                Object::Reference(r) => Ok(pdf.resolve_borrowed(r)?.as_dict().cloned()),
                _ => Ok(None),
            },
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, 0);
        assert_eq!(out[0].1.get("S"), Some(&Object::Name("D".into())));
        assert_eq!(out[1].0, 5);
        assert_eq!(out[1].1.get("S"), Some(&Object::Name("R".into())));
    }

    #[test]
    fn read_number_tree_skips_noninteger_key() {
        let mut pdf = empty_pdf();
        let mut leaf = Dictionary::new();
        leaf.insert(
            "Nums",
            Object::Array(vec![
                Object::Name("oops".into()), // non-integer key -> skip pair
                Object::Integer(1),
                Object::Integer(7),
                Object::Integer(2),
            ]),
        );
        let out: Vec<(i64, i64)> = read_number_tree(
            &mut pdf,
            Object::Dictionary(leaf),
            |_, v| Ok(v.as_integer()),
            DEFAULT_MAX_TREE_DEPTH,
        )
        .unwrap();
        assert_eq!(out, vec![(7, 2)]);
    }
```

**Step 3: Run**

Run: `cargo test -p flpdf --lib name_number_tree`
Expected: PASS (now incl. number-tree tests).

**Step 4: Commit**

```bash
git add crates/flpdf/src/name_number_tree.rs
git commit -m "feat(name_number_tree): generic number-tree reader (flpdf-9hc.18.4)"
```

---

## Task 3: `build_name_tree`

**Files:**
- Modify: `crates/flpdf/src/name_number_tree.rs`

**Step 1: Add the builder (pure; caller owns numbering + set_object)**

This MUST reproduce `rebuild_embedded_files_tree`'s node construction exactly.
Allocation order is **leaves first (in chunk order), then root** for the
multi-leaf case; single leaf = one alloc, returned as the root.

```rust
/// Build a name-tree from a **non-empty, pre-sorted** `(key, value)` slice.
///
/// Returns `(root_ref, nodes)` where `nodes` is every `(ObjectRef, Object)` the
/// caller must store via [`Pdf::set_object`]. The caller owns object numbering
/// (via `alloc`), the empty-entries case, and all catalog wiring.
///
/// Layout (qpdf-aligned, identical to the legacy embedded-files writer):
/// - `<= LEAF_MAX` entries → a single leaf node (`/Limits` + `/Names`), returned
///   as the root.
/// - `> LEAF_MAX` entries → leaves chunked by `div_ceil`, each `/Limits` +
///   `/Names`, under a root `/Limits` + `/Kids`. Leaves are allocated in order,
///   the root last.
///
/// # Panics (debug)
/// Debug-asserts `entries` is non-empty.
pub fn build_name_tree<A>(
    entries: &[(Vec<u8>, Object)],
    mut alloc: A,
) -> (ObjectRef, Vec<(ObjectRef, Object)>)
where
    A: FnMut() -> ObjectRef,
{
    debug_assert!(!entries.is_empty(), "build_name_tree requires non-empty entries");
    let mut nodes: Vec<(ObjectRef, Object)> = Vec::new();

    if entries.len() <= LEAF_MAX {
        let leaf_ref = alloc();
        nodes.push((leaf_ref, Object::Dictionary(build_leaf_dict(entries))));
        return (leaf_ref, nodes);
    }

    let n_leaves = entries.len().div_ceil(LEAF_MAX);
    let chunk_size = entries.len().div_ceil(n_leaves);
    let mut kids: Vec<Object> = Vec::with_capacity(n_leaves);
    for chunk in entries.chunks(chunk_size) {
        let leaf_ref = alloc();
        nodes.push((leaf_ref, Object::Dictionary(build_leaf_dict(chunk))));
        kids.push(Object::Reference(leaf_ref));
    }
    let first = entries.first().map(|(k, _)| k.clone()).unwrap_or_default();
    let last = entries.last().map(|(k, _)| k.clone()).unwrap_or_default();
    let mut root = Dictionary::new();
    root.insert(
        "Limits",
        Object::Array(vec![Object::String(first), Object::String(last)]),
    );
    root.insert("Kids", Object::Array(kids));
    let root_ref = alloc();
    nodes.push((root_ref, Object::Dictionary(root)));
    (root_ref, nodes)
}

/// Leaf node dict: `/Limits [first last]` + `/Names [k1 v1 ...]`.
fn build_leaf_dict(entries: &[(Vec<u8>, Object)]) -> Dictionary {
    let first = entries.first().map(|(k, _)| k.clone()).unwrap_or_default();
    let last = entries.last().map(|(k, _)| k.clone()).unwrap_or_default();
    let mut pairs: Vec<Object> = Vec::with_capacity(entries.len() * 2);
    for (key, val) in entries {
        pairs.push(Object::String(key.clone()));
        pairs.push(val.clone());
    }
    let mut dict = Dictionary::new();
    dict.insert(
        "Limits",
        Object::Array(vec![Object::String(first), Object::String(last)]),
    );
    dict.insert("Names", Object::Array(pairs));
    dict
}
```

**Step 2: Add tests**

```rust
    fn mk_entries(n: usize) -> Vec<(Vec<u8>, Object)> {
        (0..n)
            .map(|i| (format!("{i:03}").into_bytes(), Object::Reference(ObjectRef::new(1000 + i as u32, 0))))
            .collect()
    }

    #[test]
    fn build_name_tree_single_leaf_no_kids() {
        let entries = mk_entries(3);
        let mut next = 0u32;
        let (root, nodes) = build_name_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        assert_eq!(nodes.len(), 1);
        assert_eq!(root, nodes[0].0);
        let Object::Dictionary(d) = &nodes[0].1 else { panic!() };
        assert!(d.get("Kids").is_none(), "single leaf must not have /Kids");
        assert!(d.get("Names").is_some());
        assert!(d.get("Limits").is_some());
    }

    #[test]
    fn build_name_tree_multi_leaf_root_kids_alloc_order() {
        let entries = mk_entries(LEAF_MAX + 1); // 33 -> 2 leaves + root
        let mut next = 0u32;
        let (root, nodes) = build_name_tree(&entries, || {
            next += 1;
            ObjectRef::new(next, 0)
        });
        // Leaves allocated first (1,2), root last (3).
        assert_eq!(nodes.len(), 3);
        assert_eq!(root, ObjectRef::new(3, 0), "root allocated last");
        let Object::Dictionary(root_dict) = &nodes[2].1 else { panic!() };
        let Some(Object::Array(kids)) = root_dict.get("Kids") else { panic!("root needs /Kids") };
        assert_eq!(kids.len(), 2);
        assert_eq!(kids[0], Object::Reference(ObjectRef::new(1, 0)));
        assert_eq!(kids[1], Object::Reference(ObjectRef::new(2, 0)));
        // Every node carries /Limits.
        for (_, n) in &nodes {
            let Object::Dictionary(d) = n else { panic!() };
            assert!(d.get("Limits").is_some());
        }
    }
```

**Step 3: Run**

Run: `cargo test -p flpdf --lib name_number_tree`
Expected: PASS.

**Step 4: Commit**

```bash
git add crates/flpdf/src/name_number_tree.rs
git commit -m "feat(name_number_tree): pure build_name_tree matching legacy layout (flpdf-9hc.18.4)"
```

---

## Task 4: Migrate `embedded_files` readers onto `read_name_tree`

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs`

**Step 1: Replace `list_embedded_files_with_max_depth`'s tree walk**

Keep the catalog → `/Names` dict resolution (lines ~387-410). Replace Step 3
(lines ~412-430, the `ef_root` match + `collect_name_tree*` calls) with:

```rust
    // ── Step 3: walk the /EmbeddedFiles name tree (ref-only view) ─────────────
    let ef_value = match names_dict.get("EmbeddedFiles").cloned() {
        Some(v) => v,
        None => return Ok(vec![]),
    };
    crate::name_number_tree::read_name_tree(
        pdf,
        ef_value,
        |_, v| Ok(v.as_ref_id()),
        max_depth,
    )
```

(`as_ref_id` yields `Some(ObjectRef)` only for `Object::Reference`, preserving
the "skip direct-dict filespecs" semantics.)

**Step 2: Replace `collect_embedded_file_pairs_raw`'s tree walk**

Keep the catalog → `/Names` dict resolution. Replace the `match
names_dict.get("EmbeddedFiles")` block (lines ~549-560) with:

```rust
    let ef_value = match names_dict.get("EmbeddedFiles").cloned() {
        Some(v) => v,
        None => return Ok(vec![]),
    };
    crate::name_number_tree::read_name_tree(pdf, ef_value, |_, v| Ok(Some(v)), max_depth)
```

**Step 3: Delete the now-unused private walkers**

Remove `collect_name_tree`, `collect_name_tree_dict`, `collect_leaf_pairs`,
`collect_name_tree_raw`, `collect_name_tree_dict_raw`, `collect_leaf_pairs_raw`
(lines ~433-625). Remove the now-unused `use std::collections::BTreeSet;` if no
other code in the file needs it (check first — `rebuild` does not; tests may not).

**Step 4: Run the embedded_files suite**

Run: `cargo test -p flpdf --lib embedded_files`
Expected: PASS (all existing tests, no behavior change).

**Step 5: Commit**

```bash
git add crates/flpdf/src/embedded_files.rs
git commit -m "refactor(embedded_files): readers use read_name_tree (flpdf-9hc.18.4)"
```

---

## Task 5: Migrate `embedded_files` rebuild onto `build_name_tree`

**Files:**
- Modify: `crates/flpdf/src/embedded_files.rs`

**Step 1: Replace the node-building block in `rebuild_embedded_files_tree`**

Replace the `let tree_root_ref = if entries.len() <= LEAF_MAX { ... } else { ... };`
block (lines ~765-796) with:

```rust
    // ── Build the name-tree nodes (shared builder) ────────────────────────────
    let (tree_root_ref, nodes) =
        crate::name_number_tree::build_name_tree(&entries, &mut alloc);
    for (node_ref, node) in nodes {
        pdf.set_object(node_ref, node);
    }
```

`alloc` is the existing `move || -> ObjectRef` closure; passing `&mut alloc`
keeps it usable afterward for the `/Names` dict ref. The leaves-then-root alloc
order in `build_name_tree` matches the legacy code, so object numbers are
unchanged.

**Step 2: Replace `LEAF_MAX` + delete local `build_leaf_dict`**

- Delete the local `build_leaf_dict` (lines ~834-856) — now in `name_number_tree`.
- Replace the local `pub const LEAF_MAX: usize = 32;` (line 341) with a re-export
  so `embedded_files::LEAF_MAX` and the `lib.rs` re-export keep working:

```rust
pub use crate::name_number_tree::LEAF_MAX;
```

(Keep the `DEFAULT_MAX_EMBEDDED_FILES_DEPTH` const as-is.)

**Step 3: Build + run the full embedded_files suite (byte-identical bar)**

Run: `cargo test -p flpdf --lib embedded_files`
Expected: PASS — every roborev #947-#951 regression test stays green, proving
byte-identical tree output.

**Step 4: Commit**

```bash
git add crates/flpdf/src/embedded_files.rs
git commit -m "refactor(embedded_files): rebuild uses build_name_tree (flpdf-9hc.18.4)"
```

---

## Task 6: Migrate `json_inspect::walk_pagelabels` onto `read_number_tree`

**Files:**
- Modify: `crates/flpdf/src/json_inspect.rs`

**Step 1: Replace the `walk_pagelabels` call site in `build_pagelabels_section`**

Replace the `root_node` resolve + `walk_pagelabels(...)` block (lines ~1024-1039)
with a `read_number_tree` call. `pagelabels_val` (Reference or Dictionary) is
passed straight in — the generic walker resolves the root ref itself:

```rust
    let entries: Vec<(i64, Dictionary)> = crate::name_number_tree::read_number_tree(
        pdf,
        pagelabels_val,
        |pdf, v| match v {
            Object::Dictionary(d) => Ok(Some(d)),
            Object::Reference(r) => Ok(pdf.resolve_borrowed(r)?.as_dict().cloned()),
            _ => Ok(None),
        },
        DEFAULT_MAX_PAGE_TREE_DEPTH,
    )
    .map_err(ConvertError::from)?;
```

Keep the subsequent `entries.sort_by_key(...)` + JSON shaping unchanged. Note:
`entries` is no longer `mut` (it is fully built by the call); the later
`sort_by_key` needs `let mut entries = ...` — keep `let mut`.

**Step 2: Delete `walk_pagelabels`**

Remove the now-unused `walk_pagelabels` fn (lines ~929-987).

**Step 3: Run the json_inspect suite**

Run: `cargo test -p flpdf --lib json_inspect`
Expected: PASS. (PageLabels JSON output unchanged for well-formed input. The
only semantic change is depth > 100 now errors instead of truncating silently —
no test exercises that.)

**Step 4: Commit**

```bash
git add crates/flpdf/src/json_inspect.rs
git commit -m "refactor(json_inspect): pagelabels uses read_number_tree (flpdf-9hc.18.4)

Depth-exceed now returns Err (was silent truncation); pathological-only, no
observable change on well-formed PageLabels."
```

---

## Task 7: Public exports, rustdoc, final verification

**Files:**
- Modify: `crates/flpdf/src/lib.rs`

**Step 1: Re-export the public API**

Add (alphabetical, after the `linearization`/`object` block, before `outline`):

```rust
pub use name_number_tree::{
    build_name_tree, read_name_tree, read_number_tree, DEFAULT_MAX_TREE_DEPTH, LEAF_MAX,
};
```

There are now two paths to `LEAF_MAX` (via `embedded_files` re-export and via
`name_number_tree`); they are the same const. If the compiler flags an ambiguous
glob/duplicate re-export, drop `LEAF_MAX` from the `embedded_files` re-export
list in `lib.rs` (lines 103-107) since `name_number_tree::LEAF_MAX` is now the
canonical source — but `embedded_files::LEAF_MAX` (the module path) still
resolves via the `pub use` added in Task 5, so no external breakage.

**Step 2: Verify rustdoc on all new public items**

Confirm every `pub` item in `name_number_tree.rs` (`read_name_tree`,
`read_number_tree`, `build_name_tree`, `LEAF_MAX`, `DEFAULT_MAX_TREE_DEPTH`,
module header) has a `///` doc comment (added in Tasks 1-3). Run:

Run: `cargo doc -p flpdf --no-deps 2>&1 | grep -i "warning"`
Expected: no missing-docs warnings for `name_number_tree`.

**Step 3: Full crate test suite (the real bar)**

Run: `cargo test -p flpdf`
Expected: all tests pass (≥ 1027 lib + integration, 0 failures).

**Step 4: Clippy + fmt**

Run: `cargo clippy -p flpdf --all-targets -- -D warnings`
Run: `cargo fmt -p flpdf -- --check`
Expected: clean.

**Step 5: Commit**

```bash
git add crates/flpdf/src/lib.rs
git commit -m "feat(name_number_tree): export public API + docs (flpdf-9hc.18.4)"
```

---

## Done criteria

- `name_number_tree` module exists with `read_name_tree`, `read_number_tree`,
  `build_name_tree`, `LEAF_MAX`, `DEFAULT_MAX_TREE_DEPTH`, all documented.
- `embedded_files` readers + rebuild and `json_inspect::walk_pagelabels` route
  through the generic; no duplicate tree walkers remain in those files.
- `cargo test -p flpdf` green; `cargo clippy -- -D warnings` clean; `cargo fmt`
  clean. The embedded_files regression suite proves byte-identical output.
- Number-tree writer + keyed `get` deferred to flpdf-9hc.18.6 (file a note if not
  already tracked there).
