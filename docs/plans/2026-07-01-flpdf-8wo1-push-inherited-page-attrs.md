# flpdf-8wo1: Push Inherited Page Attributes Before Linearization

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Before computing a linearization plan, mutate the `/Pages` tree exactly like qpdf's
`pushInheritedAttributesToPage` (`QPDF_pages.cc:298-410`): push `/MediaBox`, `/CropBox`,
`/Resources`, `/Rotate` down to every `/Page` leaf that lacks its own copy, and strip those keys
from interior `/Pages` nodes — so `flpdf rewrite --linearize` matches qpdf byte-for-byte on
documents that use page-attribute inheritance.

**Architecture:** A new module `crates/flpdf/src/linearization/inherited_attrs.rs` implements a
single entry point, `push_inherited_attributes_to_pages`, that DFS-walks the `/Pages` tree from
`/Root /Pages`, mutating the live `Pdf` object cache in place via the existing `resolve` (owned) +
`set_object` (write-back) idiom used throughout this crate. It is called as the very first
statement inside `LinearizationPlan::from_pdf` (`crates/flpdf/src/linearization/plan.rs:712`),
before any object-ref collection happens, so every downstream step (closure computation, object
numbering, emission) sees the already-pushed tree. Non-scalar inherited values stored directly
(not by reference) are minted as new indirect objects via the crate's established
`next_object_ref` + `set_object` convention, so siblings share one object instead of duplicating
it — this is the only case that adds new objects to the document.

**Tech Stack:** Rust, the `flpdf` crate's existing `Pdf<R: Read + Seek>` object-cache API. No new
dependencies.

**Background reading before starting:** the validated design lives in the `flpdf-8wo1` beads issue
(`bd show flpdf-8wo1`) — it documents the qpdf source citations, the rejected alternatives, and why
this hooks into `from_pdf` rather than the CLI entry point. Skim it once; this plan does not repeat
the rationale, only the concrete steps.

---

### Task 0: Confirm the worktree baseline

**Step 1:** Run `cargo test -p flpdf linearization:: 2>&1 | grep -E "^test result"` and confirm
all suites report `0 failed`. This worktree was already verified clean (328 passed) at creation
time; re-run only if time has passed or you're resuming this plan in a new session.

**Step 2:** No commit — this is a sanity check, not a code change.

---

### Task 1: Module skeleton + no-op walk test

**Files:**
- Create: `crates/flpdf/src/linearization/inherited_attrs.rs`
- Modify: `crates/flpdf/src/linearization/mod.rs` (add `mod inherited_attrs;`)
- Test: inline `#[cfg(test)] mod tests` inside the new file (this module's logic is `pub(crate)`,
  so it must be unit-tested from inside the crate, not from `crates/flpdf/tests/`)

**Step 1: Write the failing test**

Create `crates/flpdf/src/linearization/inherited_attrs.rs` with just the test below (no
implementation yet — `push_inherited_attributes_to_pages` is declared but `todo!()`s):

```rust
//! Push inherited page attributes down to `/Page` leaves and strip them from
//! interior `/Pages` nodes, mirroring qpdf's `pushInheritedAttributesToPage`
//! (`QPDF_pages.cc:298-410`). Linearization runs this unconditionally before
//! computing the linearization plan — qpdf's `Lin::optimize` always passes
//! `allow_changes=true` for linearized output (`QPDF_linearization.cc:127-130`,
//! called only from `QPDFWriter::writeLinearized`). The normal (non-linearized)
//! write path never performs this step and must keep emitting `/Pages` nodes
//! verbatim.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek};

use crate::object::{Object, ObjectRef};
use crate::{Error, Pdf, Result};

/// The four page attributes a `/Pages` node may pass down to its descendants
/// (ISO 32000-2 §7.7.3.4 Table 30, "Inheritable").
const INHERITABLE_KEYS: [&[u8]; 4] = [b"MediaBox", b"CropBox", b"Resources", b"Rotate"];

/// Defensive cycle/depth bound. qpdf relies on an earlier `cache()` pass (which
/// repairs duplicate page objects and detects loops) before its own recursive
/// push runs unguarded; flpdf has no equivalent repair pass, so this function
/// guards itself. Matches the bound already used for page-tree walks elsewhere
/// in this crate ([`crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH`]).
const MAX_DEPTH: usize = crate::pages::DEFAULT_MAX_PAGE_TREE_DEPTH;

/// Push inherited attributes to every `/Page` leaf and strip them from interior
/// `/Pages` nodes, mutating `pdf` in place.
///
/// # Errors
///
/// Propagates any [`Error`] from resolving an object while walking the tree, and
/// returns [`Error::Unsupported`] if the tree exceeds [`MAX_DEPTH`].
pub(crate) fn push_inherited_attributes_to_pages<R: Read + Seek>(
    pdf: &mut Pdf<R>,
) -> Result<()> {
    let Some(root_ref) = pdf.root_ref() else {
        return Ok(());
    };
    let Some(pages_ref) = (match pdf.resolve_borrowed(root_ref)? {
        Object::Dictionary(d) => d.get_ref("Pages"),
        _ => None,
    }) else {
        return Ok(());
    };

    let mut key_ancestors: BTreeMap<&'static [u8], Vec<Object>> = BTreeMap::new();
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    push_internal(pdf, pages_ref, &mut key_ancestors, &mut visited, 0)?;
    debug_assert!(
        key_ancestors.values().all(Vec::is_empty),
        "key_ancestors not empty after pushing inherited attributes to pages"
    );
    Ok(())
}

fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    _node_ref: ObjectRef,
    _key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    _visited: &mut BTreeSet<ObjectRef>,
    _depth: usize,
) -> Result<()> {
    todo!("Task 2+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pdf;
    use std::io::Cursor;

    /// One `/Pages` node, one `/Page` leaf, no inheritable keys anywhere.
    /// Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_no_inheritable_keys() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn no_inheritable_keys_is_a_no_op() {
        let bytes = pdf_with_no_inheritable_keys();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "no inheritable keys present anywhere: no object should be minted"
        );
        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        assert!(
            page_dict.get("MediaBox").is_some(),
            "the page's own /MediaBox must be untouched"
        );
    }
}
```

**Step 2: Add the module to `linearization/mod.rs`**

In `crates/flpdf/src/linearization/mod.rs`, add the new module near the other internal modules
(it has no public exports — `push_inherited_attributes_to_pages` is `pub(crate)`, called only from
`plan.rs`):

```rust
pub mod hint_stream;
pub(crate) mod inherited_attrs;
pub mod part1;
```

(Insert alphabetically between `hint_stream` and `part1`, matching the existing list's ordering.)

**Step 3: Run test to verify it fails**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: compiles (the `todo!()` only panics if reached), but the test **panics** with
`not yet implemented: Task 2+` because `push_inherited_attributes_to_pages` calls
`push_internal`, which is unimplemented.

**Step 4: Commit the skeleton (red state is fine to commit here since the module isn't wired into
anything yet — it compiles)**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs crates/flpdf/src/linearization/mod.rs
git commit -m "feat(linearize): add push_inherited_attributes_to_pages skeleton (flpdf-8wo1)"
```

---

### Task 2: Implement the DFS walk with scalar (Rotate) push

This task makes `push_internal` real, but only the **scalar** branch (no minting yet) — this lets
us verify the walk/visited/depth machinery and the leaf-vs-Pages dispatch before adding the more
subtle non-scalar minting logic in Task 3.

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

**Step 1: Write the failing test**

Add to the `tests` module in `inherited_attrs.rs`:

```rust
    /// `/Pages` (2) has a direct, scalar `/Rotate 90`. `/Page` (3) has none.
    /// Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_inherited_scalar_rotate() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn scalar_rotate_is_copied_by_value_not_minted() {
        let bytes = pdf_with_inherited_scalar_rotate();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "a scalar inherited value must never mint a new object"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary");
        };
        assert!(
            pages_dict.get("Rotate").is_none(),
            "/Rotate must be stripped from the interior /Pages node"
        );

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        assert_eq!(
            page_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "/Rotate must be pushed to the leaf as a direct (literal) value"
        );
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: `scalar_rotate_is_copied_by_value_not_minted` panics with `not yet implemented`.
`no_inheritable_keys_is_a_no_op` also still panics (same reason) — that's fine, both fail for the
same root cause.

**Step 3: Implement `push_internal`**

Replace the `todo!()` body in `inherited_attrs.rs`:

```rust
fn push_internal<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    node_ref: ObjectRef,
    key_ancestors: &mut BTreeMap<&'static [u8], Vec<Object>>,
    visited: &mut BTreeSet<ObjectRef>,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Err(Error::Unsupported(format!(
            "page tree depth exceeds maximum of {MAX_DEPTH} at {node_ref}"
        )));
    }
    if !visited.insert(node_ref) {
        // Cycle guard: a node already on the path back to /Root. qpdf relies on
        // an earlier repair pass to make this unreachable; flpdf has none, so
        // this function defends itself. A well-formed tree never hits this.
        return Ok(());
    }

    let Object::Dictionary(mut dict) = pdf.resolve(node_ref)? else {
        return Ok(()); // Non-dictionary node: leave untouched (matches PageWalk's silent skip).
    };

    let mut own_keys: Vec<&'static [u8]> = Vec::new();
    for &key in &INHERITABLE_KEYS {
        let Some(value) = dict.get(key).cloned() else {
            continue;
        };
        // Task 3 will add the non-scalar (Array/Dictionary) minting branch here.
        // For now every value is treated as scalar (copied by value).
        key_ancestors.entry(key).or_default().push(value);
        dict.remove(key);
        own_keys.push(key);
    }

    let kids = dict.get("Kids").and_then(Object::as_array).cloned();
    pdf.set_object(node_ref, Object::Dictionary(dict));

    if let Some(kids) = kids {
        for kid in &kids {
            let Object::Reference(kid_ref) = kid else {
                continue;
            };
            let is_pages_node = matches!(
                pdf.resolve_borrowed(*kid_ref)?,
                Object::Dictionary(d)
                    if matches!(d.get("Type"), Some(Object::Name(n)) if n.as_slice() == b"Pages")
            );
            if is_pages_node {
                push_internal(pdf, *kid_ref, key_ancestors, visited, depth + 1)?;
            } else {
                let Object::Dictionary(mut leaf) = pdf.resolve(*kid_ref)? else {
                    continue;
                };
                for (&key, values) in key_ancestors.iter() {
                    if leaf.get(key).is_none() {
                        if let Some(v) = values.last() {
                            leaf.insert(key, v.clone());
                        }
                    }
                }
                pdf.set_object(*kid_ref, Object::Dictionary(leaf));
            }
        }
    }

    for key in own_keys {
        if let Some(stack) = key_ancestors.get_mut(key) {
            stack.pop();
            if stack.is_empty() {
                key_ancestors.remove(key);
            }
        }
    }
    Ok(())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: `no_inheritable_keys_is_a_no_op` and `scalar_rotate_is_copied_by_value_not_minted` both
PASS (2 passed; 0 failed).

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "feat(linearize): implement inherited-attribute DFS push (scalar case) (flpdf-8wo1)"
```

---

### Task 3: Non-scalar minting (Resources/MediaBox/CropBox)

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

**Step 1: Write the failing test**

```rust
    /// `/Pages` (2) has a direct `/Resources` dict (non-scalar). `/Page` (3) has
    /// none. Object layout: 1 Catalog, 2 Pages, 3 Page.
    fn pdf_with_inherited_direct_resources() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn direct_non_scalar_resources_is_minted_as_new_object() {
        let bytes = pdf_with_inherited_direct_resources();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count + 1,
            "a direct non-scalar inherited value must mint exactly one new object"
        );

        let pages = pdf.resolve(ObjectRef::new(2, 0)).expect("pages resolves");
        let Object::Dictionary(pages_dict) = pages else {
            panic!("pages is not a dictionary");
        };
        assert!(
            pages_dict.get("Resources").is_none(),
            "/Resources must be stripped from the interior /Pages node"
        );

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        let Some(Object::Reference(resources_ref)) = page_dict.get("Resources") else {
            panic!("/Resources must be pushed to the leaf as an indirect reference, not inline");
        };
        assert_eq!(
            resources_ref.number, 5,
            "the minted object must be the next free object number (4 was already in use)"
        );
        let minted = pdf.resolve(*resources_ref).expect("minted object resolves");
        let Object::Dictionary(minted_dict) = minted else {
            panic!("minted object is not a dictionary");
        };
        assert!(
            minted_dict.get("Font").is_some(),
            "the minted object must carry the original /Resources content"
        );
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: `direct_non_scalar_resources_is_minted_as_new_object` FAILS — currently `/Resources` is
pushed as a literal inline dict (the scalar-only code path from Task 2), not as a reference, and no
new object is minted (`object_refs().len()` stays `before_count`, not `before_count + 1`).

**Step 3: Add minting + the `next_object_ref` helper**

In `inherited_attrs.rs`, add the helper (same convention as `page_rotate.rs:617-627`):

```rust
/// Allocate a fresh indirect-object reference (the new-object idiom used across
/// the crate): one past the current highest object number.
fn next_object_ref<R: Read + Seek>(pdf: &Pdf<R>) -> Result<ObjectRef> {
    let n = pdf
        .object_refs()
        .iter()
        .map(|r| r.number)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| Error::Unsupported("object-number space exhausted".to_string()))?;
    Ok(ObjectRef::new(n, 0))
}
```

Then change the key-collection loop inside `push_internal` from:

```rust
        let Some(value) = dict.get(key).cloned() else {
            continue;
        };
        // Task 3 will add the non-scalar (Array/Dictionary) minting branch here.
        // For now every value is treated as scalar (copied by value).
        key_ancestors.entry(key).or_default().push(value);
```

to:

```rust
        let Some(value) = dict.get(key).cloned() else {
            continue;
        };
        let value = match value {
            Object::Reference(_) => value, // already indirect: descendants share this ref
            Object::Array(_) | Object::Dictionary(_) => {
                // Direct (non-indirect) non-scalar value: mint a new indirect
                // object so descendants share ONE object instead of each
                // duplicating the structure inline (mirrors qpdf's
                // makeIndirectObject call in QPDF_pages.cc:355-360).
                let new_ref = next_object_ref(pdf)?;
                pdf.set_object(new_ref, value);
                Object::Reference(new_ref)
            }
            // Integer/Real/Boolean/Name/String/Null: copy by value, no minting.
            scalar => scalar,
        };
        key_ancestors.entry(key).or_default().push(value);
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: all 3 tests so far PASS (3 passed; 0 failed).

**Step 5: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "feat(linearize): mint shared indirect object for direct non-scalar inherited values (flpdf-8wo1)"
```

**Step 6 (added after this task's own spec review flagged it): key iteration order must match
qpdf's, and needs its own test**

`INHERITABLE_KEYS`'s declaration order (`MediaBox, CropBox, Resources, Rotate`, set in Task 1) does
NOT match qpdf's actual iteration order. qpdf's `cur_pages.getKeys()` (`QPDF_Dictionary.cc`)
returns keys via a sorted `std::set<std::string>` built from the dict's backing
`std::map<std::string, QPDFObjectHandle>` (`QPDFObject_private.hh`), so `QPDF_pages.cc:346`'s push
loop visits inheritable keys alphabetically: `CropBox, MediaBox, Resources, Rotate`. This matters
for real output bytes, not just style: when a single `/Pages` node needs to mint more than one
direct non-scalar value in the same visit, mint order determines which new object gets the lower
number, and `crates/flpdf/src/linearization/plan.rs:987` sorts Part-3 objects by number — so a wrong
mint order puts different content at the same output position relative to qpdf.

Fix `INHERITABLE_KEYS` to alphabetical order:

```rust
// Alphabetical order, matching qpdf's own iteration order: `cur_pages.getKeys()`
// (QPDF_Dictionary.cc) returns keys via a sorted `std::set<std::string>`, so
// `QPDF_pages.cc`'s push loop visits inheritable keys as CropBox, MediaBox,
// Resources, Rotate. When a single node needs to mint more than one of these
// in the same visit (direct, non-indirect values), the mint order — and thus
// which new object number each gets — must match qpdf's, so this array is
// kept in that same order rather than declaration-convenient order.
const INHERITABLE_KEYS: [&[u8]; 4] = [b"CropBox", b"MediaBox", b"Resources", b"Rotate"];
```

Then add a dedicated test proving the order (mirror `pdf_with_inherited_direct_resources`'s shape:
one `/Pages` node, one `/Page` leaf, but now with TWO direct non-scalar keys on the Pages node):

```rust
    /// `/Pages` (2) has BOTH a direct `/CropBox` array and a direct `/MediaBox`
    /// array (both non-scalar, both need minting). `/Page` (3) has neither.
    /// qpdf mints in alphabetical key order (CropBox before MediaBox), so the
    /// CropBox object must get the lower object number.
    fn pdf_with_two_direct_non_scalar_keys_on_one_node() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 \
              /CropBox [0 0 100 100] /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn multiple_direct_non_scalar_keys_mint_in_qpdf_alphabetical_order() {
        let bytes = pdf_with_two_direct_non_scalar_keys_on_one_node();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(3, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary");
        };
        let Some(Object::Reference(crop_ref)) = leaf_dict.get("CropBox") else {
            panic!("/CropBox must be pushed as an indirect reference");
        };
        let Some(Object::Reference(media_ref)) = leaf_dict.get("MediaBox") else {
            panic!("/MediaBox must be pushed as an indirect reference");
        };
        assert!(
            crop_ref.number < media_ref.number,
            "/CropBox must mint before /MediaBox (qpdf's alphabetical getKeys() \
             order), got CropBox={crop_ref} MediaBox={media_ref}"
        );
    }
```

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture` — expect 4 passed; 0 failed.
Commit:

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "fix(linearize): order INHERITABLE_KEYS alphabetically to match qpdf's dict-key iteration (flpdf-8wo1)"
```

---

### Task 4: Already-indirect values are reused, not re-minted

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

No implementation change is expected for this task — the `Object::Reference(_) => value` arm added
in Task 3 already handles this. This task exists to **prove** it with a dedicated test (TDD still
applies to confirmations: write the test, watch it pass, don't skip writing it just because you
expect it to already work).

**Step 1: Write the test**

```rust
    /// `/Pages` (2) has `/Resources` as an *existing* indirect reference (4 0 R)
    /// rather than a direct dict. Two leaves (3, 5) both lack their own
    /// /Resources, so both must end up pointing at the SAME object 4 — no
    /// minting.
    fn pdf_with_already_indirect_resources_shared_by_two_pages() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn already_indirect_value_is_shared_not_reminted() {
        let bytes = pdf_with_already_indirect_resources_shared_by_two_pages();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");
        let before_count = pdf.object_refs().len();

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        assert_eq!(
            pdf.object_refs().len(),
            before_count,
            "an already-indirect inherited value must never be re-minted"
        );

        for page_num in [3u32, 5] {
            let page = pdf
                .resolve(ObjectRef::new(page_num, 0))
                .unwrap_or_else(|e| panic!("page {page_num} resolves: {e}"));
            let Object::Dictionary(page_dict) = page else {
                panic!("page {page_num} is not a dictionary");
            };
            assert_eq!(
                page_dict.get("Resources"),
                Some(&Object::Reference(ObjectRef::new(4, 0))),
                "page {page_num} must share the original object 4, not a copy"
            );
        }
    }
```

**Step 2: Run test to verify it already passes**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: 4 passed; 0 failed (including the new test, with no implementation change). If it fails,
re-examine Task 3's `match` arms — the `Object::Reference(_) => value` arm must come first.

**Step 3: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): confirm already-indirect inherited values are shared, not re-minted (flpdf-8wo1)"
```

---

### Task 5: Leaf-local value wins over inherited value

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

Also expected to already pass (the `if leaf.get(key).is_none()` guard in Task 2's implementation) —
same "write the test to prove it" discipline as Task 4.

**Step 1: Write the test**

```rust
    /// `/Pages` (2) has `/Resources` (4 0 R). The leaf `/Page` (3) has its OWN
    /// `/Resources` (5 0 R). The leaf's own value must win.
    fn pdf_with_leaf_local_resources_override() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
              /Resources 5 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 6 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F2 6 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn leaf_local_value_is_never_overwritten() {
        let bytes = pdf_with_leaf_local_resources_override();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let page = pdf.resolve(ObjectRef::new(3, 0)).expect("page resolves");
        let Object::Dictionary(page_dict) = page else {
            panic!("page is not a dictionary");
        };
        assert_eq!(
            page_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf's own /Resources (5 0 R) must NOT be replaced by the \
             ancestor's (4 0 R)"
        );
    }
```

**Step 2: Run, expect pass**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: 5 passed; 0 failed.

**Step 3: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): confirm leaf-local inherited-key value wins over ancestor (flpdf-8wo1)"
```

---

### Task 6: Nearest-ancestor wins in a 3-level tree (stack pop correctness)

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

This is the first test that actually exercises the `key_ancestors` stack-of-values-per-key
machinery across more than one level — also expected to pass against the existing implementation,
but is the one most likely to expose an off-by-one in the push/pop logic, so treat a failure here
seriously rather than assuming the test fixture is wrong.

**Step 1: Write the test**

```rust
    /// 3-level tree: grandparent /Pages (2) supplies /Resources (4 0 R).
    /// Parent /Pages (3) supplies its OWN /Resources (5 0 R), shadowing the
    /// grandparent's for everything under it. Leaf /Page (6) has neither, so
    /// it must inherit the NEAREST ancestor's value (5 0 R from the parent),
    /// not the grandparent's (4 0 R).
    fn pdf_with_three_level_nearest_ancestor_wins() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources 4 0 R >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Pages /Parent 2 0 R /Kids [6 0 R] /Count 1 \
              /Resources 5 0 R >>\nendobj\n",
        );

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(b"4 0 obj\n<< /Font << /F1 7 0 R >> >>\nendobj\n");

        let off5 = pdf.len() as u64;
        pdf.extend_from_slice(b"5 0 obj\n<< /Font << /F2 7 0 R >> >>\nendobj\n");

        let off6 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"6 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let off7 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 8\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off5:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off6:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off7:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 8 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn nearest_ancestor_value_wins_in_three_level_tree() {
        let bytes = pdf_with_three_level_nearest_ancestor_wins();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(6, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary");
        };
        assert_eq!(
            leaf_dict.get("Resources"),
            Some(&Object::Reference(ObjectRef::new(5, 0))),
            "the leaf must inherit the NEAREST ancestor's /Resources (5 0 R, \
             from the parent /Pages), not the grandparent's (4 0 R)"
        );

        // Both interior nodes must have /Resources stripped.
        let grandparent = pdf.resolve(ObjectRef::new(2, 0)).expect("grandparent resolves");
        let Object::Dictionary(gp_dict) = grandparent else {
            panic!("grandparent is not a dictionary");
        };
        assert!(gp_dict.get("Resources").is_none());

        let parent = pdf.resolve(ObjectRef::new(3, 0)).expect("parent resolves");
        let Object::Dictionary(parent_dict) = parent else {
            panic!("parent is not a dictionary");
        };
        assert!(parent_dict.get("Resources").is_none());
    }
```

**Step 2: Run, expect pass**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: 6 passed; 0 failed. If `nearest_ancestor_value_wins_in_three_level_tree` fails by
returning the grandparent's value instead of the parent's, the bug is in the `values.last()` call
(should be the most-recently-pushed = nearest ancestor) or in the pop-on-return-from-recursion
ordering — re-read the algorithm against `QPDF_pages.cc:393-396` (`key_ancestors[key].back()`).

**Step 3: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): confirm nearest-ancestor wins across 3-level Pages nesting (flpdf-8wo1)"
```

---

### Task 7: Cycle guard

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

**Step 1: Write the failing-safety test**

A malformed `/Pages` node whose `/Kids` array points back at an ancestor (or itself). This must
terminate, not hang or stack-overflow.

```rust
    /// /Pages (2)'s /Kids includes itself (2 0 R) alongside a real leaf (3 0 R).
    /// The walk must not loop forever.
    fn pdf_with_self_referential_pages_node() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn self_referential_pages_node_terminates() {
        let bytes = pdf_with_self_referential_pages_node();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        // Must return (Ok or Err), not hang. The test harness's own timeout
        // is the real backstop; this assertion documents the expected outcome.
        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(result.is_ok(), "a self-referential /Kids entry must be skipped, not error");
    }
```

**Step 1b: Malformed-node fixtures (added after Task 2's code-quality review flagged these three
branches as otherwise uncovered through Task 11)**

Three defensive branches in `push_internal` are not reachable from any well-formed fixture in
Tasks 1–6: the "resolved node is not a dictionary" bail, the "`/Kids` entry is not a `Reference`"
skip, and the "kid `Reference` resolves to a non-dictionary" skip. Cover them here, alongside the
cycle guard, since they're the same "defend against a malformed tree" concern.

```rust
    /// `/Root /Pages` (2) itself resolves to a non-dictionary object (a bare
    /// integer). The walk's very first `push_internal` call must bail via the
    /// "resolved node is not a dictionary" branch, not panic.
    fn pdf_with_non_dictionary_pages_root() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(b"2 0 obj\n42\nendobj\n");

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn non_dictionary_pages_root_is_a_no_op() {
        let bytes = pdf_with_non_dictionary_pages_root();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            result.is_ok(),
            "a non-dictionary /Pages root must be a no-op, not an error: {result:?}"
        );
    }

    /// `/Pages` (2)'s `/Kids` mixes a direct (non-reference) entry (`42`), a
    /// reference to a non-dictionary object (3, a literal string), and one
    /// real `/Page` leaf (4). Both malformed entries must be skipped; the
    /// real leaf must still receive the inherited `/Rotate`.
    fn pdf_with_malformed_kids_entries() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let off1 = pdf.len() as u64;
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let off2 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [42 3 0 R 4 0 R] /Count 1 /Rotate 90 >>\nendobj\n",
        );

        let off3 = pdf.len() as u64;
        pdf.extend_from_slice(b"3 0 obj\n(not a dictionary)\nendobj\n");

        let off4 = pdf.len() as u64;
        pdf.extend_from_slice(
            b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );

        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
        pdf.extend_from_slice(format!("{off1:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off2:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off3:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(format!("{off4:010} 00000 n \n").as_bytes());
        pdf.extend_from_slice(
            format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn malformed_kids_entries_are_skipped_valid_leaf_still_pushed() {
        let bytes = pdf_with_malformed_kids_entries();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        push_inherited_attributes_to_pages(&mut pdf).expect("push must succeed");

        let leaf = pdf.resolve(ObjectRef::new(4, 0)).expect("leaf resolves");
        let Object::Dictionary(leaf_dict) = leaf else {
            panic!("leaf is not a dictionary");
        };
        assert_eq!(
            leaf_dict.get("Rotate"),
            Some(&Object::Integer(90)),
            "the one valid leaf must still receive the inherited /Rotate despite \
             the malformed sibling entries in /Kids"
        );
    }
```

**Step 2: Run**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: all tests PASS immediately against the Task 2 implementation (every guard these tests
exercise already exists) — these tests exist to lock the behavior in and to cover, for the
patch-coverage gate: `if !visited.insert(node_ref) { return Ok(()); }` (cycle guard), the
non-dictionary-node bail, the non-`Reference` `/Kids` entry skip, and the non-dictionary-leaf skip —
none of which are reachable from any other task's well-formed fixtures.

**Step 3: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): confirm cyclic /Kids entry and malformed nodes terminate the push walk safely (flpdf-8wo1)"
```

---

### Task 8: Depth-limit error

**Files:**
- Modify: `crates/flpdf/src/linearization/inherited_attrs.rs`

**Step 1: Write the test**

Build a chain of `MAX_DEPTH + 1` nested `/Pages` nodes (no inheritable keys needed — only the depth
matters) and assert `push_inherited_attributes_to_pages` returns `Err(Error::Unsupported(_))`.

```rust
    /// A /Pages chain `MAX_DEPTH + 1` nodes deep, terminating in one /Page leaf.
    fn pdf_with_excessive_pages_depth() -> Vec<u8> {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        let depth = MAX_DEPTH + 1;
        // Object numbers: 1 = Catalog, 2..=(1+depth) = Pages chain,
        // (2+depth) = the leaf Page.
        let leaf_num = 2 + depth as u32;
        let mut offsets: Vec<u64> = Vec::with_capacity(1 + depth + 1);

        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(format!("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n").as_bytes());

        for level in 0..depth {
            let this_num = 2 + level as u32;
            let next_ref = if level + 1 == depth {
                leaf_num
            } else {
                this_num + 1
            };
            offsets.push(pdf.len() as u64);
            pdf.extend_from_slice(
                format!(
                    "{this_num} 0 obj\n<< /Type /Pages /Kids [{next_ref} 0 R] /Count 1 >>\nendobj\n"
                )
                .as_bytes(),
            );
        }

        offsets.push(pdf.len() as u64);
        pdf.extend_from_slice(
            format!(
                "{leaf_num} 0 obj\n<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
                leaf_num - 1
            )
            .as_bytes(),
        );

        let total = offsets.len() + 1; // +1 for the free-list head at object 0
        let xref_start = pdf.len() as u64;
        pdf.extend_from_slice(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes());
        for off in &offsets {
            pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!("trailer\n<< /Size {total} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n")
                .as_bytes(),
        );
        pdf
    }

    #[test]
    fn excessive_depth_returns_unsupported_error() {
        let bytes = pdf_with_excessive_pages_depth();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("valid PDF");

        let result = push_inherited_attributes_to_pages(&mut pdf);
        assert!(
            matches!(result, Err(Error::Unsupported(_))),
            "a /Pages tree deeper than MAX_DEPTH must error, not stack-overflow: {result:?}"
        );
    }
```

**Step 2: Run test to verify it fails or passes**

Run: `cargo test -p flpdf --lib inherited_attrs:: -- --nocapture`

Expected: PASSES against the Task 2 implementation (the `depth >= MAX_DEPTH` check already exists)
— same rationale as Task 7: this test locks in and covers the depth-limit branch for the
patch-coverage gate.

**Step 3: Commit**

```bash
git add crates/flpdf/src/linearization/inherited_attrs.rs
git commit -m "test(linearize): confirm excessive /Pages depth returns an error (flpdf-8wo1)"
```

---

### Task 9: Wire into `LinearizationPlan::from_pdf`

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:712-716`

**Step 1: Make the call the first statement in `from_pdf`**

In `crates/flpdf/src/linearization/plan.rs`, change:

```rust
    pub fn from_pdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        use_generate_objstm: bool,
    ) -> crate::Result<Self> {
        // ----------------------------------------------------------------
        // Step 1: collect all known object refs (Part 4 initial state).
```

to:

```rust
    pub fn from_pdf<R: Read + Seek>(
        pdf: &mut Pdf<R>,
        use_generate_objstm: bool,
    ) -> crate::Result<Self> {
        // Push inherited page attributes (/MediaBox /CropBox /Resources
        // /Rotate) down to /Page leaves and strip them from interior /Pages
        // nodes, mirroring qpdf's pushInheritedAttributesToPage — this always
        // runs for linearized output (QPDF_linearization.cc:127-130, called
        // only from QPDFWriter::writeLinearized). Must run before every step
        // below: closure computation and object-ref collection both need to
        // see the already-pushed tree, and any newly minted object must
        // already exist by the time `pdf.object_refs()` is captured.
        crate::linearization::inherited_attrs::push_inherited_attributes_to_pages(pdf)?;

        // ----------------------------------------------------------------
        // Step 1: collect all known object refs (Part 4 initial state).
```

**Step 2: Run the full linearization suite**

Run: `cargo test -p flpdf linearization:: 2>&1 | grep -E "^test result|FAILED"`

Expected: every suite still reports `0 failed`, including
`multilevel_pages_inherited_resources_join_page_closure` (`plan.rs:3790` before this edit — the
line number will have shifted slightly from the new code above it; locate it by test name, not
line number, after this edit). Per the design, this test's assertions are about final closure
membership and should hold whether the BFS in `compute_closure` reaches the resources via the
`/Parent`-chain walk or — now — directly on the leaf, since the leaf carries an explicit copy after
this push step runs first.

**If this test fails:** do not weaken the assertion. Read the actual failure — most likely cause is
the test's hand-built PDF having a `/Kids` shape `push_internal` doesn't expect (e.g. an indirect
`/Kids` array, which `dict.get("Kids").and_then(Object::as_array)` does not resolve — check whether
the test fixture uses a direct or indirect `/Kids` and adjust the fixture if needed, since this
divergence-from-qpdf's-transparent-dereferencing is a known, accepted limitation matching the
existing `PageWalk` behavior in `pages.rs`, not something to fix in this task).

**Step 3: Run the full workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -40`

Expected: all crates report `0 failed`.

**Step 4: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "feat(linearize): push inherited page attributes before computing the linearization plan (flpdf-8wo1)"
```

---

### Task 10: Idempotency regression test

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs` (add to the existing `#[cfg(test)] mod tests`)

This guards the `force_version_below_1_5` rebuild path at `writer.rs:2486`, which calls
`LinearizationPlan::from_pdf` a second time on the same `Pdf`.

**Step 1: Write the test**

Add near the other inherited-resources tests in `plan.rs`'s test module (search for
`multilevel_pages_inherited_resources_join_page_closure` and add this after it):

```rust
    /// `LinearizationPlan::from_pdf` mutates the `Pdf` in place (pushing
    /// inherited attributes). `writer.rs`'s `force_version_below_1_5` path
    /// calls it a second time on the same `Pdf` to rebuild the plan in disable
    /// mode — the second call must be a no-op for the push step: no new
    /// objects minted, same plan membership.
    #[test]
    fn from_pdf_push_step_is_idempotent_across_two_calls() {
        let bytes = two_level_pages_inherited_resources_bytes();
        let mut pdf = Pdf::open(Cursor::new(bytes)).expect("multi-level Pages PDF should parse");

        let first = LinearizationPlan::from_pdf(&mut pdf, false).expect("first plan");
        let count_after_first = pdf.object_refs().len();

        let second = LinearizationPlan::from_pdf(&mut pdf, false).expect("second plan");
        let count_after_second = pdf.object_refs().len();

        assert_eq!(
            count_after_first, count_after_second,
            "a second from_pdf call on the same Pdf must not mint any new object"
        );
        assert_eq!(
            first.all_assigned_refs(),
            second.all_assigned_refs(),
            "a second from_pdf call must produce the same set of assigned refs"
        );
    }
```

**Step 2: Run test to verify it passes**

Run: `cargo test -p flpdf --lib from_pdf_push_step_is_idempotent -- --nocapture`

Expected: PASSES (1 passed; 0 failed). If it fails on the object count assertion, the push
algorithm is not naturally idempotent — re-check that `push_internal`'s key-collection loop only
finds a key via `dict.get(key)`, which returns `None` once the key has been erased on the first
pass (no hidden state is required for idempotency; if this assumption is wrong, that's a real bug,
not a flaky test).

**Step 3: Commit**

```bash
git add crates/flpdf/src/linearization/plan.rs
git commit -m "test(linearize): confirm push-inherited-attrs step is idempotent across repeated from_pdf calls (flpdf-8wo1)"
```

---

### Task 11: qpdf-oracle byte-identical emission test

**Files:**
- Create: a new fixture-driving test, placed alongside the project's existing `qpdf-zlib-compat`-gated
  byte-identical tests. First locate the existing pattern:

**Step 1: Find the existing oracle-comparison convention**

Run: `grep -rl "qpdf-zlib-compat" crates/flpdf/tests/ crates/flpdf-cli/tests/ | head -5` and read one
matching file fully to find the exact harness function used to shell out to the real `qpdf` binary
and diff bytes (likely something like `run_qpdf_and_compare` or similar — use whatever helper
already exists rather than reinventing one). Also run:
`grep -n "qpdf-zlib-compat" .github/workflows/ci.yml` to see how such tests are explicitly listed
for CI (per this repo's convention: byte-identical tests under this feature are NOT picked up by a
wildcard — they must be added to the CI job's explicit test list).

**Step 2: Write the test**

Using the harness found in Step 1, add a test (in the same file, or a new
`crates/flpdf/tests/linearize_inherited_attrs_qpdf_compat.rs` if the existing files are organized
one-file-per-feature) that:

1. Builds (or loads from a fixture file) a PDF where the `/Pages` node holds a direct
   (non-indirect) `/Resources` dict and the single `/Page` leaf has no local `/Resources` — same
   shape as `pdf_with_inherited_direct_resources()` from Task 3, but written as a `.pdf` file under
   `crates/flpdf/tests/fixtures/` (or wherever this repo's other oracle fixtures live — check via
   `find crates/flpdf/tests -iname "*.pdf" | head -5` for the convention) if the existing harness
   expects a file path rather than in-memory bytes.
2. Runs `flpdf rewrite --linearize` (in-process via the library, or via the CLI binary — match
   whatever the existing harness does) on it.
3. Runs the real `qpdf --linearize` binary on the same input (the existing harness should already
   wrap this).
4. Asserts the two outputs are byte-identical.

**Step 3: Run under the feature gate**

Run: `cargo test -p flpdf --features qpdf-zlib-compat <new_test_name> -- --nocapture`

Expected: PASS. If qpdf is not installed in this environment, the existing harness should already
skip gracefully (check its behavior in Step 1's reading) — do not add new skip logic if one already
exists.

**Step 4: Add to the CI explicit test list**

Edit `.github/workflows/ci.yml` to add the new test name to wherever the existing
`qpdf-zlib-compat`-gated tests are explicitly enumerated (per this repo's established convention —
these are not picked up automatically).

**Step 5: Commit**

```bash
git add crates/flpdf/tests/ .github/workflows/ci.yml
git commit -m "test(linearize): add qpdf-oracle byte-identical test for inherited /Resources emission (flpdf-8wo1)"
```

---

### Task 12: Full verification gate and patch coverage

**Step 1: Format, lint, full test**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Fix anything these report before proceeding.

**Step 2: Patch coverage**

Per this repo's `CLAUDE.md`, run (after all commits above are in place — the script diffs against
`HEAD`, so an uncommitted tree gives a false read):

```bash
scripts/patch-coverage.sh --base main
```

`flpdf` changed lines must be 100% covered. If any line is reported uncovered, either add a test
exercising it or, only if genuinely untestable, mark it with `// cov:ignore: <reason>` and note the
reason in the eventual PR description.

**Step 3: Qualitative check (per `CLAUDE.md`, beyond the coverage number)**

Re-read each new test added in Tasks 1–10 and confirm each assertion is substantive (checks a
specific object ref, a specific dictionary key's presence/absence, or a specific value) — not a
bare "doesn't panic". This plan's tests were written to that standard; re-verify after any edits
made while fixing coverage gaps.

**Step 4: Final commit if anything changed during this task**

```bash
git add -A
git commit -m "chore(linearize): fix coverage gaps for inherited-attribute push (flpdf-8wo1)"
```

(Skip this commit if Steps 1–3 required no changes.)

---

## Out of scope (explicitly, per the saved design)

- Normal (non-linearized) `flpdf rewrite` must NOT push/strip inherited attributes — qpdf's
  `writeStandard()` never calls `optimize()`. Do not generalize this fix.
- Do not delete the existing `/Parent`-chain walk in `compute_closure` (`plan.rs`, commit
  `eadf33b`) — it becomes redundant after this fix but is left as defensive dead code per the
  project's "no implicit deviation" rule. Removing it is a separate, explicitly-scoped cleanup.
- Do not attempt to fix indirect `/Kids` array handling (qpdf dereferences transparently; flpdf's
  existing `PageWalk` and this new `push_internal` both require `/Kids` to be a direct array) — this
  is a pre-existing, separate gap shared with `pages.rs::PageWalk`, out of scope here.
- Do not attempt to fix indirect `/Type` handling either (qpdf's `isDictionaryOfType` dereferences
  transparently; `push_internal`'s `is_pages_node` check does not) — same rationale as the `/Kids`
  gap above: it matches `PageWalk`'s existing precedent in `pages.rs`, is a pre-existing, separate
  gap, and is out of scope here.
