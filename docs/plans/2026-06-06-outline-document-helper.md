# OutlineDocumentHelper Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `Pdf::outline()` returning an `OutlineDocumentHelper` — a qpdf-faithful, cycle-safe, iterable handle over the PDF outline (bookmark) tree (beads `flpdf-9hc.18.5`).

**Architecture:** A thin wrapper `OutlineDocumentHelper<'a, R> { pdf: &'a mut Pdf<R> }` (mirrors `AcroFormDocumentHelper`) eagerly materializes the `/Outlines` tree into an owned `Vec<OutlineNode>`. Each `OutlineNode` owns `children: Vec<OutlineNode>` and carries `title`, raw `/Count`, `parent` ref, and a resolved `dest`. `iter`/`walk` traverse the owned tree pre-order. Reuses the `BTreeSet`-cycle-detection + depth-cap skeleton from `outline.rs::walk_outline` (re-implemented locally; that fn is private) and `name_number_tree::read_name_tree` for named-destination resolution. The existing flat `outline.rs` API is left untouched.

**Tech Stack:** Rust (edition 2021, MSRV 1.87), workspace crate `flpdf`. Tests are in-memory PDFs built with a `build_pdf(objects, root)` helper. Quality gates: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test -p flpdf`.

**Review rules (`.claude/rules/pdf-rust-review-patterns.md`):** (1) avoid needless `.clone()` of resolved owned objects; (2) `resolve` indirect refs before matching `/Title`/`/Count`/`/First`/`/Next`/dest values; (3) keep `/Count` as `i64`, never `as usize` an external int; (4) bound traversal with `visited: BTreeSet<ObjectRef>` + depth cap, never follow `/Parent`.

**Working directory:** `/home/ubuntu/flpdf/.worktrees/flpdf-9hc-18-5-outline-helper`
All paths below are relative to that worktree root. Run all `cargo` commands from there.

---

## Reference: existing code to mirror

- Helper template + `Pdf` extension: `crates/flpdf/src/acroform_document_helper.rs` (struct at top; `impl Pdf { pub fn acroform(...) }` near line 541).
- Tree-walk skeleton (cycle + depth + `/First`/`/Next`): `crates/flpdf/src/outline.rs:65-108` (`walk_outline`). DO NOT modify this file.
- Dest extraction patterns (depth-bounded `/Dest`, `/A /S /GoTo /D`, array/dict `/D`): `crates/flpdf/src/outline_dest_remap.rs:864-1030` (`remap_item_dest`, `remap_dest_value_depth`, `dest_page_ref_resolved_depth`, const `MAX_DEST_RESOLVE_DEPTH = 64`).
- Name-tree reader for named dests: `crates/flpdf/src/name_number_tree.rs:37` (`read_name_tree`).
- Object/Dictionary API: `crates/flpdf/src/object.rs` — `Dictionary::get`, `get_ref`, `Object::as_integer`, `as_name`, `as_string`, `as_ref_id`, `as_dict`, `as_array`; `Pdf::resolve`, `resolve_borrowed` (`crates/flpdf/src/reader.rs`).
- Test PDF builder to copy: `crates/flpdf/tests/acroform_document_helper_tests.rs:6-33` (`build_pdf`).
- Module/export sites: `crates/flpdf/src/lib.rs:62` (`pub mod outline;`), `:122` (`pub use outline::OutlineItem;`).

---

## Task 1: Module skeleton + `Pdf::outline()` + `has_outlines()`

**Files:**

- Create: `crates/flpdf/src/outline_document_helper.rs`
- Modify: `crates/flpdf/src/lib.rs` (add `pub mod outline_document_helper;` after line 62; add re-export after line 122)
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs` (create)

**Step 1: Write the failing test**

Create `crates/flpdf/tests/outline_document_helper_tests.rs`:

```rust
//! Integration tests for [`flpdf::OutlineDocumentHelper`].

use flpdf::{Object, ObjectRef, Pdf};
use std::collections::BTreeMap;
use std::io::Cursor;

/// Build a minimal cross-reffed PDF from `(objnum, body)` pairs.
fn build_pdf(objects: &[(u32, &str)], root: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.7\n".to_vec();
    let mut offsets: BTreeMap<u32, u64> = BTreeMap::new();
    let max = objects.iter().map(|(n, _)| *n).max().unwrap_or(0);
    for (n, body) in objects {
        offsets.insert(*n, out.len() as u64);
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let xref_start = out.len() as u64;
    let size = max + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=max {
        match offsets.get(&n) {
            Some(offset) => out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()),
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root {root} 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// Catalog + pages + a two-level outline:
///   root(4) -> First A(5)
///   A(5)    -> First A1(6); A1 has dest [3 0 R /Fit]
///   A(5)    -> Next  B(7);  B has /Count 2
fn outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 7 0 R /Count 2 >>"),
            (5, "<< /Title (A) /Parent 4 0 R /First 6 0 R /Last 6 0 R /Next 7 0 R /Count 1 >>"),
            (6, "<< /Title (A1) /Parent 5 0 R /Dest [3 0 R /Fit] >>"),
            (7, "<< /Title (B) /Parent 4 0 R /Prev 5 0 R /Count 2 >>"),
        ],
        1,
    )
}

fn no_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        ],
        1,
    )
}

#[test]
fn has_outlines_true_when_present() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    assert!(pdf.outline().has_outlines().unwrap());
}

#[test]
fn has_outlines_false_when_absent() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(!pdf.outline().has_outlines().unwrap());
}
```

**Step 2: Run to verify it fails**

Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -20`
Expected: compile error — `no method named outline` / unresolved `OutlineDocumentHelper`.

**Step 3: Write minimal implementation**

Create `crates/flpdf/src/outline_document_helper.rs`:

```rust
//! High-level outline (`/Outlines`) document helper.
//!
//! [`OutlineDocumentHelper`] wraps a `&mut Pdf<R>` and exposes a cycle-safe,
//! iterable handle over the document outline (bookmark) tree, mirroring qpdf's
//! `QPDFOutlineDocumentHelper`. It materializes the tree into owned
//! [`OutlineNode`]s; navigation (`children`, `parent`, `count`, `dest`) lives on
//! each node, mirroring `QPDFOutlineObjectHelper`.

use crate::{Object, ObjectRef, Pdf, Result};
use std::io::{Read, Seek};

/// High-level outline helper for a document. See module docs.
pub struct OutlineDocumentHelper<'a, R: Read + Seek> {
    pdf: &'a mut Pdf<R>,
}

impl<'a, R: Read + Seek> OutlineDocumentHelper<'a, R> {
    /// Wrap a document for outline access. Prefer [`Pdf::outline`].
    pub fn new(pdf: &'a mut Pdf<R>) -> Self {
        Self { pdf }
    }

    /// Return `true` if the catalog has an `/Outlines` dictionary with at least
    /// one top-level item (a resolvable `/First`). Mirrors qpdf `hasOutlines`.
    pub fn has_outlines(&mut self) -> Result<bool> {
        Ok(self.outline_root_first()?.is_some())
    }

    /// Resolve the catalog `/Outlines` dict's `/First` child ref, if any.
    fn outline_root_first(&mut self) -> Result<Option<ObjectRef>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Object::Dictionary(catalog) = self.pdf.resolve_borrowed(catalog_ref)? else {
            return Ok(None);
        };
        let Some(outlines_ref) = catalog.get_ref("Outlines") else {
            return Ok(None);
        };
        let Object::Dictionary(root) = self.pdf.resolve_borrowed(outlines_ref)? else {
            return Ok(None);
        };
        Ok(root.get_ref("First"))
    }
}

impl<R: Read + Seek> Pdf<R> {
    /// Return a high-level outline helper for this document.
    pub fn outline(&mut self) -> OutlineDocumentHelper<'_, R> {
        OutlineDocumentHelper::new(self)
    }
}
```

In `crates/flpdf/src/lib.rs` add after line 62 (`pub mod outline_dest_remap;` block):

```rust
pub mod outline_document_helper;
```

and after line 122 (`pub use outline::OutlineItem;`):

```rust
pub use outline_document_helper::OutlineDocumentHelper;
```

**Step 4: Run to verify it passes**

Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -20`
Expected: 2 passed.

**Step 5: Commit**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/src/lib.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "feat(outline_document_helper): module skeleton + Pdf::outline() + has_outlines (flpdf-9hc.18.5)"
```

---

## Task 2: `OutlineNode` type + `get_root()` core materialization

Materialize the tree with `title` (resolved, empty if absent — qpdf), raw `/Count` as `i64`, `parent: Option<ObjectRef>`, owned `children`. No dest yet (Task 5).

**Files:**

- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs`

**Step 1: Write the failing test** (append to test file)

```rust
#[test]
fn get_root_materializes_tree_with_titles_counts_parents() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();

    // Two top-level nodes: A, B.
    assert_eq!(roots.len(), 2);
    assert_eq!(roots[0].title, "A");
    assert_eq!(roots[0].depth, 0);
    assert_eq!(roots[0].parent, Some(ObjectRef::new(4, 0)));
    assert_eq!(roots[0].count, 1);
    assert_eq!(roots[1].title, "B");
    assert_eq!(roots[1].count, 2);

    // A has one child A1.
    assert_eq!(roots[0].children.len(), 1);
    let a1 = &roots[0].children[0];
    assert_eq!(a1.title, "A1");
    assert_eq!(a1.depth, 1);
    assert_eq!(a1.parent, Some(ObjectRef::new(5, 0)));
    assert_eq!(a1.count, 0); // /Count absent -> 0 (qpdf)
    assert_eq!(a1.object_ref, ObjectRef::new(6, 0));
}

#[test]
fn get_root_empty_when_no_outline() {
    let mut pdf = Pdf::open(Cursor::new(no_outline_pdf())).unwrap();
    assert!(pdf.outline().get_root().unwrap().is_empty());
}
```

**Step 2: Run to verify it fails**
Run: `cargo test -p flpdf --test outline_document_helper_tests get_root 2>&1 | tail -20`
Expected: compile error — no `get_root`, no `OutlineNode`.

**Step 3: Write minimal implementation**

Add the const + node type near the top of `outline_document_helper.rs` (after the `use`):

```rust
use std::collections::BTreeSet;

/// Default recursion limit for outline materialization. Matches
/// [`crate::outline::DEFAULT_MAX_OUTLINE_DEPTH`]. True unbounded/iterative deep
/// walking (1000+ levels, cycle diagnostics) is tracked by flpdf-9hc.14.7.
pub const DEFAULT_MAX_OUTLINE_DEPTH: usize = crate::outline::DEFAULT_MAX_OUTLINE_DEPTH;

/// One materialized node of the outline tree (a bookmark).
///
/// Mirrors qpdf's `QPDFOutlineObjectHelper`. `children` are the resolved
/// `/First`→`/Next` chain; `parent` is the owning node's ref (`None` for
/// top-level items). `count` is the raw `/Count` value (0 when absent), whose
/// sign indicates open/closed per ISO 32000-1 §12.3.3.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineNode {
    /// Object ref of this outline item dictionary.
    pub object_ref: ObjectRef,
    /// Zero for top-level items, increasing per nesting level.
    pub depth: usize,
    /// `/Title` decoded with `from_utf8_lossy`; empty string when absent.
    pub title: String,
    /// Raw `/Count` value; `0` when absent.
    pub count: i64,
    /// Parent item ref; `None` for top-level items.
    pub parent: Option<ObjectRef>,
    /// Resolved destination (set in Task 5); `None` until then.
    pub dest: Option<Dest>,
    /// Child nodes in `/First`→`/Next` order.
    pub children: Vec<OutlineNode>,
}

/// A resolved explicit destination, e.g. `[pageRef /Fit ...]`. Mirrors the
/// array form qpdf `getDest` yields after resolving `/Dest`, `/A /GoTo /D`, and
/// named destinations.
#[derive(Debug, Clone, PartialEq)]
pub struct Dest {
    /// The explicit destination array. Element 0 is normally the page ref.
    pub array: Vec<Object>,
}

impl Dest {
    /// The destination page ref (array element 0), if it is an indirect ref.
    /// Mirrors qpdf `getDestPage`.
    pub fn page(&self) -> Option<ObjectRef> {
        self.array.first().and_then(Object::as_ref_id)
    }
}
```

Add to the `impl OutlineDocumentHelper` block:

```rust
    /// Materialize and return the top-level outline nodes (qpdf
    /// `getTopLevelOutlines`). "root" is this top-level vector; the `/Outlines`
    /// dict itself is not a navigable item and is not wrapped.
    pub fn get_root(&mut self) -> Result<Vec<OutlineNode>> {
        self.get_root_with_max_depth(DEFAULT_MAX_OUTLINE_DEPTH)
    }

    /// Like [`get_root`](Self::get_root) with a caller-supplied recursion limit.
    /// Returns [`crate::Error::Unsupported`] if the limit is exceeded.
    pub fn get_root_with_max_depth(&mut self, max_depth: usize) -> Result<Vec<OutlineNode>> {
        let Some(first) = self.outline_root_first()? else {
            return Ok(Vec::new());
        };
        let mut visited = BTreeSet::new();
        self.build_siblings(first, 0, None, &mut visited, max_depth)
    }

    /// Build a `/First`→`/Next` sibling chain into owned nodes.
    fn build_siblings(
        &mut self,
        start: ObjectRef,
        depth: usize,
        parent: Option<ObjectRef>,
        visited: &mut BTreeSet<ObjectRef>,
        max_depth: usize,
    ) -> Result<Vec<OutlineNode>> {
        if depth >= max_depth {
            return Err(crate::Error::Unsupported(format!(
                "outline depth exceeds maximum of {max_depth} at {start}"
            )));
        }
        let mut nodes = Vec::new();
        let mut current = Some(start);
        while let Some(current_ref) = current {
            if !visited.insert(current_ref) {
                break; // cycle — stop this chain
            }
            let Object::Dictionary(dict) = self.pdf.resolve_borrowed(current_ref)? else {
                break;
            };
            // IMPORTANT (borrow order): `dict` borrows `self.pdf` (it is a
            // `resolve_borrowed` reference). Extract EVERY value we need into
            // owned locals here, ending the `dict` borrow, BEFORE any
            // `self.pdf.resolve(...)` call below — otherwise the borrow checker
            // rejects it. Task 5 adds `dest_src`/`action_src` to this block.
            let title = read_title(dict.get("Title"));
            let first = dict.get_ref("First");
            let next = dict.get_ref("Next");
            let count_src = dict.get("Count").cloned();
            // `dict` (and thus the &mut self.pdf borrow) is no longer used past
            // this point — owned values only from here on.
            let count = resolve_int(self.pdf, count_src)?.unwrap_or(0);

            let children = match first {
                Some(first) => {
                    self.build_siblings(first, depth + 1, Some(current_ref), visited, max_depth)?
                }
                None => Vec::new(),
            };

            nodes.push(OutlineNode {
                object_ref: current_ref,
                depth,
                title,
                count,
                parent,
                dest: None,
                children,
            });
            current = next;
        }
        Ok(nodes)
    }
```

Add free helpers at the bottom of the module:

```rust
/// `/Title` decode: qpdf yields an empty string when absent. A resolved
/// indirect string is handled by the caller passing the already-borrowed value;
/// here we only decode direct `Object::String`. (Indirect `/Title` is resolved
/// in Task-2b refinement; see note.)
fn read_title(value: Option<&Object>) -> String {
    match value {
        Some(Object::String(bytes)) => String::from_utf8_lossy(bytes).into_owned(),
        Some(_) | None => String::new(),
    }
}

/// Resolve one level of indirection and read an integer (review rule 2/3).
fn resolve_int<R: Read + Seek>(pdf: &mut Pdf<R>, value: Option<Object>) -> Result<Option<i64>> {
    match value {
        Some(Object::Reference(r)) => Ok(pdf.resolve(r)?.as_integer()),
        Some(other) => Ok(other.as_integer()),
        None => Ok(None),
    }
}
```

> **Note on indirect `/Title`:** `read_title` above only handles a direct
> string to keep the borrow simple. If the integration corpus needs indirect
> `/Title` support, resolve it the same way as `/Count`: clone `dict.get("Title")`,
> end the borrow, then `resolve` and decode. Add a test first.

**Step 4: Run to verify it passes**
Run: `cargo test -p flpdf --test outline_document_helper_tests get_root 2>&1 | tail -20`
Expected: 2 passed. Then run clippy:
`cargo clippy -p flpdf --all-targets -- -D warnings 2>&1 | tail -15` — must be clean.

**Step 5: Commit**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "feat(outline_document_helper): OutlineNode + get_root materialization (flpdf-9hc.18.5)"
```

---

## Task 3: `iter()` and `walk(visitor)`

Pre-order flatten + closure visitor over the owned tree.

**Files:**

- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs`

**Step 1: Write the failing test** (append)

```rust
#[test]
fn iter_yields_preorder() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let titles: Vec<String> = pdf.outline().iter().unwrap().map(|n| n.title).collect();
    assert_eq!(titles, vec!["A", "A1", "B"]); // pre-order: A, its child A1, then B
}

#[test]
fn walk_visits_preorder_with_depth() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let mut seen: Vec<(String, usize)> = Vec::new();
    pdf.outline()
        .walk(|node, depth| seen.push((node.title.clone(), depth)))
        .unwrap();
    assert_eq!(
        seen,
        vec![
            ("A".to_string(), 0),
            ("A1".to_string(), 1),
            ("B".to_string(), 0),
        ]
    );
}
```

**Step 2: Run to verify it fails**
Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -20`
Expected: compile error — no `iter`/`walk`.

**Step 3: Write minimal implementation**

Add to the `impl OutlineDocumentHelper` block:

```rust
    /// Pre-order iterator over every materialized node (owned).
    pub fn iter(&mut self) -> Result<impl Iterator<Item = OutlineNode>> {
        let roots = self.get_root()?;
        let mut flat = Vec::new();
        for node in &roots {
            flatten_preorder(node, &mut flat);
        }
        Ok(flat.into_iter())
    }

    /// Visit every node pre-order, passing `(node, depth)` to `visitor`.
    pub fn walk<F: FnMut(&OutlineNode, usize)>(&mut self, mut visitor: F) -> Result<()> {
        let roots = self.get_root()?;
        for node in &roots {
            walk_node(node, &mut visitor);
        }
        Ok(())
    }
```

Add free helpers at the bottom:

```rust
fn flatten_preorder(node: &OutlineNode, out: &mut Vec<OutlineNode>) {
    out.push(OutlineNode {
        children: Vec::new(),
        ..node.clone()
    });
    for child in &node.children {
        flatten_preorder(child, out);
    }
}

fn walk_node<F: FnMut(&OutlineNode, usize)>(node: &OutlineNode, visitor: &mut F) {
    visitor(node, node.depth);
    for child in &node.children {
        walk_node(child, visitor);
    }
}
```

> **Design note:** `iter()` yields nodes with `children` cleared (the flattened
> view is linear; `depth` conveys structure). `walk()` borrows the real nodes so
> the visitor sees populated `children`. This keeps `iter`'s item type simple.

**Step 4: Run to verify it passes**
Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -20`
Expected: 6 passed total. Clippy clean.

**Step 5: Commit**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "feat(outline_document_helper): iter + walk pre-order traversal (flpdf-9hc.18.5)"
```

---

## Task 4: Depth cap + cycle safety + moderate-depth integration test

Satisfies the acceptance criterion "integration test exercises deep outline traversal" at moderate depth (~30); true 1000-deep/iterative is deferred to flpdf-9hc.14.7.

**Files:**

- Test: `crates/flpdf/tests/outline_document_helper_tests.rs`

**Step 1: Write the failing test** (append) — programmatic deep + cyclic fixtures

```rust
/// Build a linear chain of `n` nested outline items (each is the sole child of
/// the previous). Object numbers: catalog 1, pages 2, page 3, outlines 4,
/// items 5..5+n. Returns PDF bytes.
fn deep_outline_pdf(n: u32) -> Vec<u8> {
    let mut objs: Vec<(u32, String)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>".to_string()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string()),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_string()),
    ];
    // outline root (4) points First/Last at first item (5).
    objs.push((4, format!("<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>")));
    for i in 0..n {
        let num = 5 + i;
        let parent = if i == 0 { 4 } else { num - 1 };
        let mut body = format!("<< /Title (L{i}) /Parent {parent} 0 R");
        if i + 1 < n {
            let child = num + 1;
            body.push_str(&format!(" /First {child} 0 R /Last {child} 0 R"));
        }
        body.push_str(" >>");
        objs.push((num, body));
    }
    let refs: Vec<(u32, &str)> = objs.iter().map(|(n, s)| (*n, s.as_str())).collect();
    build_pdf(&refs, 1)
}

#[test]
fn deep_outline_walks_to_full_depth() {
    let mut pdf = Pdf::open(Cursor::new(deep_outline_pdf(30))).unwrap();
    let count = pdf.outline().iter().unwrap().count();
    assert_eq!(count, 30);
    // deepest node is at depth 29
    let max_depth = pdf
        .outline()
        .iter()
        .unwrap()
        .map(|n| n.depth)
        .max()
        .unwrap();
    assert_eq!(max_depth, 29);
}

#[test]
fn depth_cap_is_enforced() {
    let mut pdf = Pdf::open(Cursor::new(deep_outline_pdf(10))).unwrap();
    let err = pdf.outline().get_root_with_max_depth(5);
    assert!(err.is_err(), "expected depth-cap error, got {err:?}");
}

/// Outline with a /Next cycle: 5 -> Next 6 -> Next 5 ...
fn cyclic_outline_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 6 0 R /Count 2 >>"),
            (5, "<< /Title (X) /Parent 4 0 R /Next 6 0 R >>"),
            (6, "<< /Title (Y) /Parent 4 0 R /Next 5 0 R >>"), // cycle back to 5
        ],
        1,
    )
}

#[test]
fn cyclic_outline_terminates() {
    let mut pdf = Pdf::open(Cursor::new(cyclic_outline_pdf())).unwrap();
    let titles: Vec<String> = pdf.outline().iter().unwrap().map(|n| n.title).collect();
    // Visits X and Y once each, then the cycle back to 5 is cut by `visited`.
    assert_eq!(titles, vec!["X", "Y"]);
}
```

**Step 2: Run to verify it fails / passes appropriately**
Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -25`
Expected: these compile and PASS with the Task 2/3 implementation (the cap + cycle logic already exists). If `deep_outline_walks_to_full_depth` overflows the stack at depth 30, that is a real signal the recursive walker is too fragile — STOP and report (do not silently raise/lower depth). At 30 it must not overflow.

**Step 3: Implementation** — none expected (verifies Task 2/3 logic). If a test legitimately exposes a bug, fix in `outline_document_helper.rs` with the minimal change.

**Step 4: Confirm pass**
Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -25`
Expected: 9 passed total.

**Step 5: Commit**

```bash
git add crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "test(outline_document_helper): deep (~30), depth-cap, and cycle traversal (flpdf-9hc.18.5)"
```

---

## Task 5: Destination resolution — explicit `/Dest` + `/A /GoTo /D` + `dest_page()`

Resolve `dest` for each node (no named-dest tree yet — Task 6). Mirrors qpdf `getDest` for the GoTo + explicit-array cases.

**Files:**

- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs`

**Step 1: Write the failing test** (append)

```rust
#[test]
fn dest_from_explicit_dest_array() {
    let mut pdf = Pdf::open(Cursor::new(outline_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    let a1 = &roots[0].children[0]; // A1 has /Dest [3 0 R /Fit]
    let dest = a1.dest.as_ref().expect("A1 should have a dest");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
    // Nodes without a dest stay None.
    assert!(roots[1].dest.is_none()); // B
}

/// Outline item whose destination is a GoTo action: /A << /S /GoTo /D [3 0 R /Fit] >>.
fn action_dest_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (Act) /Parent 4 0 R /A << /S /GoTo /D [3 0 R /Fit] >> >>"),
        ],
        1,
    )
}

#[test]
fn dest_from_goto_action() {
    let mut pdf = Pdf::open(Cursor::new(action_dest_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0].dest.as_ref().expect("GoTo action should yield a dest");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
}
```

**Step 2: Run to verify it fails**
Run: `cargo test -p flpdf --test outline_document_helper_tests dest 2>&1 | tail -20`
Expected: FAIL — `a1.dest` is `None`.

**Step 3: Write minimal implementation**

Add `MAX_DEST_RESOLVE_DEPTH` const near the top:

```rust
/// Indirection/`/D` nesting bound when resolving a destination. Mirrors the
/// constant in `outline_dest_remap`. Only exists to make malformed/cyclic
/// `/D` structures terminate instead of overflowing the stack.
const MAX_DEST_RESOLVE_DEPTH: usize = 64;
```

In `build_siblings`, extend the owned-extraction block (the one that already
clones `count_src` while `dict` is still borrowed) to also clone the dest
sources, THEN — after the `dict` borrow has ended — resolve the dest alongside
`count`:

```rust
            // ...inside the owned-extraction block, while `dict` is borrowed:
            let count_src = dict.get("Count").cloned();
            let dest_src = dict.get("Dest").cloned();
            let action_src = dict.get("A").cloned();
            // `dict` borrow ends here — owned values only below.
            let count = resolve_int(self.pdf, count_src)?.unwrap_or(0);
            let dest = self.resolve_node_dest(dest_src, action_src)?;
```

and set `dest` in the pushed `OutlineNode { ... dest, ... }` (replace the
`dest: None` from Task 2 with the resolved `dest`).

Add to the `impl OutlineDocumentHelper` block:

```rust
    /// Resolve a node's destination from `/Dest`, else a `/A` GoTo action's `/D`.
    /// Named/string destinations are resolved in Task 6 (`resolve_named_dest`).
    fn resolve_node_dest(
        &mut self,
        dest: Option<Object>,
        action: Option<Object>,
    ) -> Result<Option<Dest>> {
        // 1. /Dest takes precedence.
        if let Some(d) = dest {
            if let Some(found) = self.dest_from_value(&d, MAX_DEST_RESOLVE_DEPTH)? {
                return Ok(Some(found));
            }
        }
        // 2. /A with /S /GoTo -> /D.
        if let Some(a) = action {
            let action_obj = match a {
                Object::Reference(r) => self.pdf.resolve(r)?,
                other => other,
            };
            if let Some(adict) = action_obj.as_dict() {
                let is_goto = matches!(adict.get("S"), Some(Object::Name(n)) if n == b"GoTo");
                if is_goto {
                    if let Some(d) = adict.get("D").cloned() {
                        if let Some(found) = self.dest_from_value(&d, MAX_DEST_RESOLVE_DEPTH)? {
                            return Ok(Some(found));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Resolve a destination value (array / indirect / dict `/D`) to a [`Dest`].
    /// Named (Name/String) destinations return `None` here (Task 6 adds them).
    fn dest_from_value(&mut self, value: &Object, depth: usize) -> Result<Option<Dest>> {
        if depth == 0 {
            return Ok(None);
        }
        match value {
            Object::Array(arr) => Ok(Some(Dest { array: arr.clone() })),
            Object::Reference(r) => {
                let concrete = self.pdf.resolve(*r)?;
                self.dest_from_value(&concrete, depth - 1)
            }
            Object::Dictionary(d) => match d.get("D").cloned() {
                Some(inner) => self.dest_from_value(&inner, depth - 1),
                None => Ok(None),
            },
            _ => Ok(None), // Name/String named dest — Task 6
        }
    }
```

**Step 4: Run to verify it passes**
Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -25`
Expected: 11 passed total. Clippy clean.

**Step 5: Commit**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "feat(outline_document_helper): resolve explicit + GoTo-action destinations (flpdf-9hc.18.5)"
```

---

## Task 6: Named-destination resolution (`/Names`→`/Dests` + legacy `/Dests`)

Resolve Name/String destinations against the catalog name tree and legacy dict — the bulk of qpdf `getDest`'s `resolveNamedDest`.

**Files:**

- Modify: `crates/flpdf/src/outline_document_helper.rs`
- Test: `crates/flpdf/tests/outline_document_helper_tests.rs`

**Step 1: Write the failing test** (append) — both modern (string + name tree) and legacy (name + `/Dests` dict)

```rust
/// Modern named dest: outline /Dest (named) is a string resolved via
/// catalog /Names /Dests name tree. Name tree leaf maps (mydest) -> [3 0 R /Fit].
fn named_dest_nametree_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Names 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (N) /Parent 4 0 R /Dest (mydest) >>"),
            (8, "<< /Dests 9 0 R >>"),
            (9, "<< /Names [(mydest) [3 0 R /Fit]] >>"),
        ],
        1,
    )
}

#[test]
fn dest_from_named_nametree() {
    let mut pdf = Pdf::open(Cursor::new(named_dest_nametree_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0].dest.as_ref().expect("named dest should resolve");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
}

/// Legacy named dest: /Dest is a Name (/mydest) resolved via catalog /Dests
/// dictionary whose value is << /D [3 0 R /Fit] >>.
fn named_dest_legacy_pdf() -> Vec<u8> {
    build_pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R /Outlines 4 0 R /Dests 8 0 R >>"),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
            (4, "<< /Type /Outlines /First 5 0 R /Last 5 0 R /Count 1 >>"),
            (5, "<< /Title (L) /Parent 4 0 R /Dest /mydest >>"),
            (8, "<< /mydest << /D [3 0 R /Fit] >> >>"),
        ],
        1,
    )
}

#[test]
fn dest_from_named_legacy() {
    let mut pdf = Pdf::open(Cursor::new(named_dest_legacy_pdf())).unwrap();
    let roots = pdf.outline().get_root().unwrap();
    let dest = roots[0].dest.as_ref().expect("legacy named dest should resolve");
    assert_eq!(dest.page(), Some(ObjectRef::new(3, 0)));
}
```

**Step 2: Run to verify it fails**
Run: `cargo test -p flpdf --test outline_document_helper_tests named 2>&1 | tail -20`
Expected: FAIL — `dest` is `None` for Name/String.

**Step 3: Write minimal implementation**

In `dest_from_value`, replace the `_ => Ok(None)` arm:

```rust
            Object::Name(name) => self.resolve_named_dest(name.clone()),
            Object::String(name) => self.resolve_named_dest(name.clone()),
            _ => Ok(None),
```

Add `use crate::name_number_tree::read_name_tree;` to the imports (top of file) and a const for the name-tree depth cap, then add to the `impl OutlineDocumentHelper` block:

```rust
    /// Resolve a named destination `name` to an explicit [`Dest`].
    ///
    /// Tries the modern catalog `/Names`→`/Dests` name tree first (PDF 1.2),
    /// then the legacy catalog `/Dests` dictionary (PDF 1.1). A name-tree or
    /// `/Dests` value may be the dest array directly or a `<< /D array >>` dict.
    fn resolve_named_dest(&mut self, name: Vec<u8>) -> Result<Option<Dest>> {
        // 1. Modern: catalog /Names /Dests -> name tree.
        if let Some(root) = self.catalog_ref("Names")? {
            if let Object::Dictionary(names) = self.pdf.resolve(root)? {
                if let Some(dests_root) = names.get("Dests").cloned() {
                    let entries = read_name_tree(
                        self.pdf,
                        dests_root,
                        |_pdf, value| Ok(Some(value)),
                        DEFAULT_MAX_OUTLINE_DEPTH,
                    )?;
                    for (key, value) in entries {
                        if key == name {
                            return self.dest_from_value(&value, MAX_DEST_RESOLVE_DEPTH);
                        }
                    }
                }
            }
        }
        // 2. Legacy: catalog /Dests dict.
        if let Some(dests_ref) = self.catalog_ref("Dests")? {
            if let Object::Dictionary(dests) = self.pdf.resolve(dests_ref)? {
                if let Some(value) = dests.get(&name).cloned() {
                    return self.dest_from_value(&value, MAX_DEST_RESOLVE_DEPTH);
                }
            }
        }
        Ok(None)
    }

    /// Resolve a catalog key to an object ref (the key's value as an indirect
    /// ref). Returns `None` if absent or not a reference.
    fn catalog_ref(&mut self, key: &str) -> Result<Option<ObjectRef>> {
        let Some(catalog_ref) = self.pdf.root_ref() else {
            return Ok(None);
        };
        let Object::Dictionary(catalog) = self.pdf.resolve_borrowed(catalog_ref)? else {
            return Ok(None);
        };
        Ok(catalog.get_ref(key))
    }
```

> **Implementation notes:**
>
> - `read_name_tree` takes the `/Dests` name-tree root `Object` (a ref or inline
>   node dict) and a `decode` closure; returning `Ok(Some(value))` keeps the raw
>   dest value (array or `/D` dict), which `dest_from_value` then normalizes.
> - `Dictionary::get(&name)` accepts `impl AsRef<[u8]>`, so a `Vec<u8>` key works
>   for the legacy `/Dests` lookup (the leading `/` is not part of the key bytes).
> - If `catalog_ref("Names")` returns the names dict inline (non-indirect), adjust
>   to read it via `catalog.get("Names")` resolved; the fixtures use an indirect
>   ref so the ref path is exercised. Add a direct-dict test only if the corpus
>   needs it (YAGNI).

**Step 4: Run to verify it passes**
Run: `cargo test -p flpdf --test outline_document_helper_tests 2>&1 | tail -25`
Expected: 13 passed total. Clippy clean.

**Step 5: Commit**

```bash
git add crates/flpdf/src/outline_document_helper.rs crates/flpdf/tests/outline_document_helper_tests.rs
git commit -m "feat(outline_document_helper): resolve named destinations (name tree + legacy) (flpdf-9hc.18.5)"
```

---

## Task 7: Exports, rustdoc polish, full quality gate

Satisfy the "API documented" acceptance criterion and finalize.

**Files:**

- Modify: `crates/flpdf/src/lib.rs` (extend the re-export to the new public types)
- Modify: `crates/flpdf/src/outline_document_helper.rs` (rustdoc with a usage example)

**Step 1: Extend exports**

In `crates/flpdf/src/lib.rs`, change the Task-1 re-export to:

```rust
pub use outline_document_helper::{Dest, OutlineDocumentHelper, OutlineNode};
```

**Step 2: Add a rustdoc usage example** to the module header in `outline_document_helper.rs` (a doctest):

```rust
//! # Example
//!
//! ```no_run
//! use flpdf::Pdf;
//! use std::io::Cursor;
//!
//! # fn f(bytes: Vec<u8>) -> flpdf::Result<()> {
//! let mut pdf = Pdf::open(Cursor::new(bytes))?;
//! if pdf.outline().has_outlines()? {
//!     pdf.outline().walk(|node, depth| {
//!         println!("{:indent$}{}", "", node.title, indent = depth * 2);
//!     })?;
//! }
//! # Ok(())
//! # }
//! ```
```

**Step 3: Run the full quality gate**

```bash
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings 2>&1 | tail -20
cargo test -p flpdf 2>&1 | tail -30
cargo test --doc -p flpdf 2>&1 | tail -15
```

Expected: fmt clean, clippy clean, all flpdf tests pass (existing + 13 new), doctest compiles.

**Step 4: Verify existing outline behavior is untouched**
Run: `cargo test -p flpdf --test inspection_tests 2>&1 | tail -12`
Expected: 7 passed (including `outline_items_returns_titles_in_pre_order` — the `<untitled>` flat API is unchanged).

**Step 5: Commit**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/outline_document_helper.rs
git commit -m "docs(outline_document_helper): export types + rustdoc example; finalize (flpdf-9hc.18.5)"
```

---

## Done criteria

- `Pdf::outline()` returns `OutlineDocumentHelper` with `has_outlines`, `get_root`, `get_root_with_max_depth`, `iter`, `walk`.
- `OutlineNode` exposes `object_ref`, `depth`, `title`, `count` (raw `/Count`), `parent`, `dest`, `children`; `Dest::page()` / `OutlineNode.dest` resolved from `/Dest`, `/A GoTo /D`, and named destinations (name tree + legacy).
- 13 new integration tests pass; deep (~30), depth-cap, and cycle cases covered; existing tests untouched.
- `fmt` + `clippy -D warnings` + `cargo test -p flpdf` + doctest all green.
- True 1000-deep / iterative / cycle-diagnostic walking remains with flpdf-9hc.14.7 (dependency recorded).
