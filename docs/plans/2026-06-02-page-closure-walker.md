# Per-page Resource Closure Walker Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement `page_object_closure()` — a BFS traversal that returns the transitive set of all `ObjectRef`s reachable from a given PDF page reference.

**Architecture:** New module `page_closure.rs` exposes a single free function `page_object_closure(pdf, page_ref) -> Result<BTreeSet<ObjectRef>>`. A helper `collect_refs_in_object()` recursively extracts `ObjectRef` values from any `Object` variant. A BFS loop drives the traversal using a visited set to prevent cycles.

**Tech Stack:** Rust, `std::collections::{BTreeSet, VecDeque}`, existing `flpdf` types (`Object`, `ObjectRef`, `Pdf`, `Error`, `Result`).

---

### Task 1: Create the test file with a failing test for the basic single-page case

**Files:**
- Create: `crates/flpdf/tests/page_closure_tests.rs`

**Step 1: Write the failing test**

Create `crates/flpdf/tests/page_closure_tests.rs` with:

```rust
//! Integration tests for [`flpdf::page_closure::page_object_closure`].

use flpdf::{page_closure, pages, Object, ObjectRef, Pdf};
use std::io::Cursor;

// ---------------------------------------------------------------------------
// Minimal PDF builder helpers (copied pattern from page_object_helper_tests)
// ---------------------------------------------------------------------------

/// Build a minimal single-page PDF with no resources.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page
fn build_minimal_pdf() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = out.len() as u64;
    out.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n");

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 4\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \n{off3:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer = format!("trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn closure_contains_page_ref_itself() {
    let data = build_minimal_pdf();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let page_ref = page_refs[0];

    let closure = page_closure::page_object_closure(&mut pdf, page_ref).unwrap();

    assert!(closure.contains(&page_ref), "closure must contain the page ref itself");
}
```

**Step 2: Add the test to Cargo.toml**

Open `crates/flpdf/Cargo.toml` and add at the end:

```toml
[[test]]
name = "page_closure_tests"
path = "tests/page_closure_tests.rs"
```

**Step 3: Run the test to confirm it fails**

```bash
cargo test --test page_closure_tests 2>&1 | tail -20
```

Expected: compile error — `page_closure` module not found.

---

### Task 2: Create the module skeleton so the test compiles but logic is unimplemented

**Files:**
- Create: `crates/flpdf/src/page_closure.rs`
- Modify: `crates/flpdf/src/lib.rs`

**Step 1: Create `page_closure.rs` with a stub**

Create `crates/flpdf/src/page_closure.rs`:

```rust
//! Per-page transitive object closure.
//!
//! Given a page `ObjectRef`, [`page_object_closure`] computes the complete set
//! of indirect objects reachable from that page via reference chains.  The
//! result is the minimal set of objects needed to reproduce the page's content
//! and resources in isolation.

use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Seek};

/// Return the transitive closure of all [`ObjectRef`]s reachable from `page_ref`.
///
/// Traverses the object graph breadth-first, following every
/// [`Object::Reference`] encountered.  The page dictionary itself, its content
/// streams, `/Resources` subtree (fonts, XObjects, colour spaces, patterns,
/// ExtGStates, properties, shadings), annotations, and all nested references
/// are included automatically — no special-casing per resource type is needed
/// because the BFS follows every reference link regardless of semantic role.
///
/// Cycles are handled via the `visited` set: each `ObjectRef` is resolved at
/// most once.
///
/// # Errors
///
/// Returns [`Err`] only if [`Pdf::resolve`] fails for an object (e.g. corrupt
/// or missing xref entry).
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{page_closure, pages, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// let page_refs = pages::page_refs(&mut pdf)?;
/// if let Some(&page_ref) = page_refs.first() {
///     let closure = page_closure::page_object_closure(&mut pdf, page_ref)?;
///     println!("page 1 needs {} objects", closure.len());
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn page_object_closure<R: Read + Seek>(
    _pdf: &mut Pdf<R>,
    _page_ref: ObjectRef,
) -> Result<BTreeSet<ObjectRef>> {
    unimplemented!()
}
```

**Step 2: Declare the module in lib.rs**

Open `crates/flpdf/src/lib.rs`. Find the block of `pub mod` declarations (around line 40–79). Insert in alphabetical order after `page_collate`:

```rust
pub mod page_closure;
```

**Step 3: Run the test to confirm it now compiles but panics**

```bash
cargo test --test page_closure_tests 2>&1 | tail -10
```

Expected: test runs and fails with "not implemented".

---

### Task 3: Implement `collect_refs_in_object` helper and the BFS loop

**Files:**
- Modify: `crates/flpdf/src/page_closure.rs`

**Step 1: Replace the stub with the full implementation**

Replace the entire contents of `crates/flpdf/src/page_closure.rs` with:

```rust
//! Per-page transitive object closure.
//!
//! Given a page `ObjectRef`, [`page_object_closure`] computes the complete set
//! of indirect objects reachable from that page via reference chains.  The
//! result is the minimal set of objects needed to reproduce the page's content
//! and resources in isolation.

use crate::{Object, ObjectRef, Pdf, Result};
use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Seek};

/// Return the transitive closure of all [`ObjectRef`]s reachable from `page_ref`.
///
/// Traverses the object graph breadth-first, following every
/// [`Object::Reference`] encountered.  The page dictionary itself, its content
/// streams, `/Resources` subtree (fonts, XObjects, colour spaces, patterns,
/// ExtGStates, properties, shadings), annotations, and all nested references
/// are included automatically — no special-casing per resource type is needed
/// because the BFS follows every reference link regardless of semantic role.
///
/// Cycles are handled via the `visited` set: each `ObjectRef` is resolved at
/// most once.
///
/// # Errors
///
/// Returns [`Err`] only if [`Pdf::resolve`] fails for an object (e.g. corrupt
/// or missing xref entry).
///
/// # Examples
///
/// ```no_run
/// use std::fs::File;
/// use std::io::BufReader;
/// use flpdf::{page_closure, pages, Pdf};
///
/// let mut pdf = Pdf::open(BufReader::new(File::open("input.pdf")?))?;
/// let page_refs = pages::page_refs(&mut pdf)?;
/// if let Some(&page_ref) = page_refs.first() {
///     let closure = page_closure::page_object_closure(&mut pdf, page_ref)?;
///     println!("page 1 needs {} objects", closure.len());
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn page_object_closure<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_ref: ObjectRef,
) -> Result<BTreeSet<ObjectRef>> {
    let mut visited: BTreeSet<ObjectRef> = BTreeSet::new();
    let mut queue: VecDeque<ObjectRef> = VecDeque::new();

    visited.insert(page_ref);
    queue.push_back(page_ref);

    while let Some(current_ref) = queue.pop_front() {
        let obj = pdf.resolve(current_ref)?;
        let mut refs_found = Vec::new();
        collect_refs_in_object(&obj, &mut refs_found);
        for r in refs_found {
            if visited.insert(r) {
                queue.push_back(r);
            }
        }
    }

    Ok(visited)
}

/// Recursively collect every [`ObjectRef`] embedded in `obj` into `out`.
///
/// Stream data bytes are opaque binary and cannot contain indirect references,
/// so only the stream dictionary is traversed.
fn collect_refs_in_object(obj: &Object, out: &mut Vec<ObjectRef>) {
    match obj {
        Object::Reference(r) => out.push(*r),
        Object::Array(items) => {
            for item in items {
                collect_refs_in_object(item, out);
            }
        }
        Object::Dictionary(dict) => {
            for (_key, value) in dict.iter() {
                collect_refs_in_object(value, out);
            }
        }
        Object::Stream(stream) => {
            for (_key, value) in stream.dict.iter() {
                collect_refs_in_object(value, out);
            }
        }
        // Scalar types carry no references.
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
}
```

**Step 2: Run the single existing test to see it pass**

```bash
cargo test --test page_closure_tests closure_contains_page_ref_itself 2>&1 | tail -10
```

Expected: `test closure_contains_page_ref_itself ... ok`

---

### Task 4: Test that the closure is non-trivial (includes referenced objects)

**Files:**
- Modify: `crates/flpdf/tests/page_closure_tests.rs`

**Step 1: Add a PDF builder with an explicit font reference and a new test**

Append to `crates/flpdf/tests/page_closure_tests.rs`:

```rust
/// Build a single-page PDF where the page references a Font resource (object 4).
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page  (has /Resources with font ref)
///   4 0 R  Font dictionary
fn build_pdf_with_font() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 4 0 R >> >> >>\nendobj\n",
    );

    let off4 = out.len() as u64;
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 5\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer =
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[test]
fn closure_includes_font_resource() {
    let data = build_pdf_with_font();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let page_ref = page_refs[0];
    let font_ref = ObjectRef::new(4, 0);

    let closure = page_closure::page_object_closure(&mut pdf, page_ref).unwrap();

    assert!(closure.contains(&font_ref), "closure must include font object 4 0 R");
}
```

**Step 2: Run the new test**

```bash
cargo test --test page_closure_tests closure_includes_font_resource 2>&1 | tail -10
```

Expected: `test closure_includes_font_resource ... ok`

---

### Task 5: Test that shared objects appear in both pages' closures on a multi-page PDF

**Files:**
- Modify: `crates/flpdf/tests/page_closure_tests.rs`

**Step 1: Add a two-page PDF builder and test**

Append to `crates/flpdf/tests/page_closure_tests.rs`:

```rust
/// Build a two-page PDF where both pages share the same font (object 5).
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page 1 (references font 5 0 R)
///   4 0 R  Page 2 (references font 5 0 R)
///   5 0 R  Font (shared)
fn build_two_page_pdf_shared_font() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n",
    );

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );

    let off4 = out.len() as u64;
    out.extend_from_slice(
        b"4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 5 0 R >> >> >>\nendobj\n",
    );

    let off5 = out.len() as u64;
    out.extend_from_slice(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
    );

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer =
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[test]
fn shared_object_appears_in_both_page_closures() {
    let data = build_two_page_pdf_shared_font();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let font_ref = ObjectRef::new(5, 0);

    let closure_p1 = page_closure::page_object_closure(&mut pdf, page_refs[0]).unwrap();
    let closure_p2 = page_closure::page_object_closure(&mut pdf, page_refs[1]).unwrap();

    assert!(closure_p1.contains(&font_ref), "page 1 closure must contain shared font");
    assert!(closure_p2.contains(&font_ref), "page 2 closure must contain shared font");
    // Page 1 must not contain page 2's ref and vice-versa.
    assert!(!closure_p1.contains(&page_refs[1]), "page 1 closure must not contain page 2 ref");
    assert!(!closure_p2.contains(&page_refs[0]), "page 2 closure must not contain page 1 ref");
}
```

**Step 2: Run the test**

```bash
cargo test --test page_closure_tests shared_object_appears_in_both_page_closures 2>&1 | tail -10
```

Expected: `test shared_object_appears_in_both_page_closures ... ok`

---

### Task 6: Test that cycles do not loop forever

**Files:**
- Modify: `crates/flpdf/tests/page_closure_tests.rs`

**Step 1: Build a PDF where two objects mutually reference each other, then add test**

Append to `crates/flpdf/tests/page_closure_tests.rs`:

```rust
/// Build a PDF with a synthetic reference cycle: object 4 references object 5,
/// object 5 references object 4.  The page (object 3) references object 4.
///
/// Object layout:
///   1 0 R  Catalog
///   2 0 R  Pages
///   3 0 R  Page (has /Resources /XObject << /Im0 4 0 R >>)
///   4 0 R  dictionary containing 5 0 R
///   5 0 R  dictionary containing 4 0 R  ← cycle
fn build_pdf_with_cycle() -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();

    let off1 = out.len() as u64;
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    let off2 = out.len() as u64;
    out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    let off3 = out.len() as u64;
    out.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /XObject << /Im0 4 0 R >> >> >>\nendobj\n",
    );

    let off4 = out.len() as u64;
    out.extend_from_slice(b"4 0 obj\n<< /Next 5 0 R >>\nendobj\n");

    let off5 = out.len() as u64;
    out.extend_from_slice(b"5 0 obj\n<< /Next 4 0 R >>\nendobj\n");

    let xref_start = out.len() as u64;
    out.extend_from_slice(
        format!(
            "xref\n0 6\n\
             0000000000 65535 f \n\
             {off1:010} 00000 n \n\
             {off2:010} 00000 n \n\
             {off3:010} 00000 n \n\
             {off4:010} 00000 n \n\
             {off5:010} 00000 n \n"
        )
        .as_bytes(),
    );
    let trailer =
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n");
    out.extend_from_slice(trailer.as_bytes());
    out
}

#[test]
fn cycle_does_not_loop_forever() {
    let data = build_pdf_with_cycle();
    let mut pdf = Pdf::open_mem(&data).unwrap();
    let page_refs = pages::page_refs(&mut pdf).unwrap();
    let page_ref = page_refs[0];

    // This must terminate; if it loops forever, the test hangs.
    let closure = page_closure::page_object_closure(&mut pdf, page_ref).unwrap();

    assert!(closure.contains(&ObjectRef::new(4, 0)));
    assert!(closure.contains(&ObjectRef::new(5, 0)));
}
```

**Step 2: Run all tests**

```bash
cargo test --test page_closure_tests 2>&1 | tail -15
```

Expected: all 4 tests pass.

---

### Task 7: Run the full test suite and commit

**Step 1: Run all tests**

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass (including the 4 new page_closure tests), 0 failures.

**Step 2: Commit**

```bash
git add crates/flpdf/src/page_closure.rs \
        crates/flpdf/src/lib.rs \
        crates/flpdf/tests/page_closure_tests.rs \
        crates/flpdf/Cargo.toml \
        docs/plans/2026-06-02-page-closure-walker.md
git commit -m "feat(page_closure): add page_object_closure BFS traversal

Implements the per-page transitive object closure walker (flpdf-5h5.1).
Given a page ObjectRef, page_object_closure() returns a BTreeSet of all
ObjectRefs reachable via BFS, covering content streams, /Resources subtrees,
annotations, and all nested references. Cycles are handled by a visited set."
```
