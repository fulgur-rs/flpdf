# qpdf Null-Aware Writer Traversal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port qpdf 11.9.0's null-aware dictionary visibility into flpdf's existing standard BFS and compressible-object DFS so plain Disable, Preserve, Generate, and then linearized output match qpdf byte-for-byte.

**Architecture:** Keep qpdf's two existing algorithms separate: `CatalogFirstRenumber`/`GenerateRenumber` remain queue-based standard enqueue walks, while `compressible_objgens` remains a stack-based DFS. Extract one chain-aware `isNull()` equivalent plus sorted visible-dictionary snapshots, then use those primitives at the existing dictionary branches and serializers.

**Tech Stack:** Rust workspace, qpdf 11.9.0 oracle, committed PDF fixtures/goldens, `qpdf-zlib-compat`, Cargo tests/clippy, `scripts/patch-coverage.sh`, Git dependent branches and draft PRs.

## Global Constraints

- qpdf 11.9.0 tag `v11.9.0` at commit `3b97c9bd266b7c32ea36d3536e22dab77412886d` is the sole behavior and byte oracle.
- Do not replace qpdf's standard-enqueue BFS or `getCompressibleObjGens` DFS with a generalized graph walk.
- Dictionary and stream-dictionary values that qpdf `isNull()` resolves to null are invisible; arrays retain every element.
- Preserve first-visit order, key order, array order, ObjStm even splitting, member order, container-first numbering, and xref type-2 rows.
- The source `Pdf` object graph must not be mutated by null visibility analysis.
- This stack covers unencrypted, non-QDF full rewrites. QDF, encryption, and copy-encryption are tracked by `flpdf-9hc.42`.
- Every behavior change requires a qpdf 11.9.0 fixture and committed byte oracle before implementation.
- Changed-line coverage is a 100% gate and must be measured from each layer's clean committed `HEAD`.
- Tracking order is `flpdf-9hc.40` → `flpdf-v58c` → `flpdf-9hc.41`.

---

## File Map

- Create `crates/flpdf/src/qpdf_null.rs`
  - Own the qpdf-compatible chain-aware null predicate and visible dictionary snapshots.
- Modify `crates/flpdf/src/lib.rs`
  - Register the crate-private `qpdf_null` module.
- Modify `crates/flpdf/src/json_inspect.rs`
  - Reuse the shared predicate and remove its duplicate JSON-only implementation.
- Modify `crates/flpdf/src/rewrite_renumber.rs`
  - Keep existing BFS data structures; make direct dictionary recursion and plain reference rewriting qpdf-null-aware.
- Modify `crates/flpdf/src/writer/object_streams.rs`
  - Keep the existing DFS stack and eligibility order; source dictionary children from qpdf-visible entries.
- Modify `crates/flpdf/src/writer.rs`
  - Select the qpdf-null-aware renumber/remap path only for plain non-QDF output in this stack.
- Modify `crates/flpdf/src/linearization/plan.rs`
  - Replace `!live` null approximations in the dependent linearization layer.
- Modify `crates/flpdf/src/linearization/writer.rs`
  - Reuse the shared predicate when omitting dictionary keys and rewriting array references.
- Create `tests/fixtures/compat/null-visible-matrix.pdf`
  - Classic-xref matrix for direct null, object 0, missing, free, REAL-null, holder chains, nested positions, and shared dict/array refs.
- Create `tests/fixtures/compat/null-visible-matrix-objstm.pdf`
  - ObjStm-bearing form used to pin Preserve source membership.
- Create `tests/fixtures/compat/null-visible-split-boundary.pdf`
  - Generate fixture with qpdf-null edges around the 100-member grouping boundary.
- Create `tests/fixtures/compat/null-visible-cycle.pdf`
  - Isolated holder-cycle oracle so warning/cache behavior cannot affect the main matrix.
- Create `crates/flpdf/tests/cmp_null_visibility_tests.rs`
  - Strict Disable/Preserve/Generate and later linearized byte comparisons.
- Modify `tests/golden/regenerate.sh`
  - Reproducibly generate the four source fixtures with inline Python, generate
    all qpdf 11.9.0 references, and validate them.
- Create output files under `tests/golden/references/null-visible-*`
  - Committed qpdf oracle bytes; never create these from flpdf output.

---

### Task 1: Shared qpdf `isNull()` Semantics

**Issue/branch:** `flpdf-9hc.40` on
`refactor/flpdf-v58c-qpdf-null-walk`

**Files:**
- Create: `crates/flpdf/src/qpdf_null.rs`
- Modify: `crates/flpdf/src/lib.rs`
- Modify: `crates/flpdf/src/json_inspect.rs`
- Test: `crates/flpdf/src/qpdf_null.rs`

**Interfaces:**
- Consumes: `Pdf::resolve_qpdf_json_object`, `Object`, `ObjectRef`,
  `Dictionary`. The existing resolver name is historical; its fallback to
  qpdf-parsed xref-stream objects is required to preserve JSON behavior and is
  also the closest local equivalent of qpdf's object lookup.
- Produces:
  - `pub(crate) fn reference_is_valid(reference: ObjectRef) -> bool`
  - `pub(crate) fn reference_is_null<R: Read + Seek>(pdf: &mut Pdf<R>, reference: ObjectRef) -> Result<bool>`
  - `pub(crate) fn value_is_null<R: Read + Seek>(pdf: &mut Pdf<R>, value: &Object) -> Result<bool>`
  - `pub(crate) fn snapshot_entries(dict: &Dictionary, skip_length: bool) -> Vec<(Vec<u8>, Object)>`
  - `pub(crate) fn visible_entries<R: Read + Seek>(pdf: &mut Pdf<R>, entries: Vec<(Vec<u8>, Object)>) -> Result<Vec<(Vec<u8>, Object)>>`

- [ ] **Step 1: Claim the bottom-layer issue**

Run:

```bash
bd update flpdf-9hc.40 --claim
bd show flpdf-9hc.40
```

Expected: `flpdf-9hc.40` is `IN_PROGRESS`; `flpdf-v58c` remains blocked by it.

- [ ] **Step 2: Register the new private module and write failing null tests**

Add to `crates/flpdf/src/lib.rs` in alphabetical order:

```rust
pub(crate) mod qpdf_null;
```

Create `crates/flpdf/src/qpdf_null.rs` with imports, function declarations that
return `false`, and focused tests. Use the same explicit-xref fixture style as
`rewrite_renumber.rs::tests::build_raw_pdf`. The test graph must contain:

```text
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R >> endobj
4 0 obj null endobj
5 0 obj 4 0 R endobj
6 0 obj 7 0 R endobj
7 0 obj 6 0 R endobj
```

The xref must also contain a free entry for object 8; object 99 is absent.
Write these exact assertions:

```rust
#[test]
fn qpdf_null_classifies_direct_missing_free_real_and_holder_values() {
    let mut pdf = open_null_fixture();
    assert!(value_is_null(&mut pdf, &Object::Null).unwrap());
    assert!(reference_is_null(&mut pdf, ObjectRef::new(0, 0)).unwrap());
    assert!(reference_is_null(&mut pdf, ObjectRef::new(99, 0)).unwrap());
    assert!(reference_is_null(&mut pdf, ObjectRef::new(8, 0)).unwrap());
    assert!(reference_is_null(&mut pdf, ObjectRef::new(4, 0)).unwrap());
    assert!(reference_is_null(&mut pdf, ObjectRef::new(5, 0)).unwrap());
    assert!(!reference_is_null(&mut pdf, ObjectRef::new(1, 0)).unwrap());
}

#[test]
fn qpdf_null_terminates_holder_cycles_as_null() {
    let mut pdf = open_null_fixture();
    assert!(reference_is_null(&mut pdf, ObjectRef::new(6, 0)).unwrap());
}
```

- [ ] **Step 3: Run the focused tests and verify RED**

Run:

```bash
cargo test -p flpdf qpdf_null::tests -- --nocapture
```

Expected: both tests fail because the stub returns `false`.

- [ ] **Step 4: Implement the chain-aware predicate**

Implement the two functions exactly around full-reference identity and a
visited set:

```rust
use crate::{Dictionary, Object, ObjectRef, Pdf, Result};
use std::collections::BTreeSet;
use std::io::{Read, Seek};

pub(crate) fn reference_is_valid(reference: ObjectRef) -> bool {
    reference.number > 0 && reference.generation < u16::MAX
}

pub(crate) fn reference_is_null<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    reference: ObjectRef,
) -> Result<bool> {
    if !reference_is_valid(reference) {
        return Ok(true);
    }
    let mut current = reference;
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current) {
            return Ok(true);
        }
        match pdf.resolve_qpdf_json_object(current)? {
            Object::Null => return Ok(true),
            Object::Reference(next) => current = next,
            _ => return Ok(false),
        }
    }
}

pub(crate) fn value_is_null<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    value: &Object,
) -> Result<bool> {
    match value {
        Object::Null => Ok(true),
        Object::Reference(reference) => reference_is_null(pdf, *reference),
        _ => Ok(false),
    }
}

pub(crate) fn snapshot_entries(
    dict: &Dictionary,
    skip_length: bool,
) -> Vec<(Vec<u8>, Object)> {
    dict.iter()
        .filter(|(key, _)| !(skip_length && *key == b"Length"))
        .map(|(key, value)| (key.to_vec(), value.clone()))
        .collect()
}

pub(crate) fn visible_entries<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    entries: Vec<(Vec<u8>, Object)>,
) -> Result<Vec<(Vec<u8>, Object)>> {
    let mut visible = Vec::with_capacity(entries.len());
    for (key, value) in entries {
        if !value_is_null(pdf, &value)? {
            visible.push((key, value));
        }
    }
    Ok(visible)
}
```

The two-stage snapshot API is intentional: callers finish borrowing a cached
dictionary before `value_is_null` mutably resolves another object.

- [ ] **Step 5: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p flpdf qpdf_null::tests -- --nocapture
```

Expected: all `qpdf_null::tests` pass.

- [ ] **Step 6: Make JSON inspection reuse the shared predicate**

Delete `qpdf_reference_is_valid` and `qpdf_reference_resolves_to_null` from
`json_inspect.rs`. Replace its dictionary filter with:

```rust
let omit = crate::qpdf_null::value_is_null(pdf, value)?;
```

Replace the separate invalid-reference guard in `qpdf_pdf_object_to_json` with:

```rust
Object::Reference(reference)
    if !crate::qpdf_null::reference_is_valid(*reference) =>
{
    Ok(JsonValue::Null)
}
```

`ConvertError` already implements conversion from `crate::Error`; keep the
existing `?` conversion and do not change JSON error text.

- [ ] **Step 7: Verify JSON and core behavior**

Run:

```bash
cargo test -p flpdf qpdf_null::tests -- --nocapture
cargo test -p flpdf json_inspect::tests -- --nocapture
cargo fmt --all -- --check
```

Expected: all selected tests pass and fmt reports no diff.

- [ ] **Step 8: Commit the semantic core**

```bash
git add crates/flpdf/src/lib.rs crates/flpdf/src/qpdf_null.rs crates/flpdf/src/json_inspect.rs
git commit -m "refactor(writer): share qpdf null resolution"
```

---

### Task 2: Port Standard BFS and Plain Disable/Preserve Serialization

**Issue/branch:** `flpdf-9hc.40` on
`refactor/flpdf-v58c-qpdf-null-walk`

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Create: `tests/fixtures/compat/null-visible-matrix.pdf`
- Create: `tests/fixtures/compat/null-visible-matrix-objstm.pdf`
- Create: `tests/fixtures/compat/null-visible-cycle.pdf`
- Create: `crates/flpdf/tests/cmp_null_visibility_tests.rs`
- Modify: `tests/golden/regenerate.sh`
- Create: `tests/golden/references/null-visible-matrix/disable.pdf`
- Create: `tests/golden/references/null-visible-matrix-objstm/preserve.pdf`
- Create: `tests/golden/references/null-visible-cycle/disable.pdf`

**Interfaces:**
- Consumes: Task 1's `qpdf_null::{snapshot_entries, visible_entries, value_is_null}`.
- Produces:
  - `collect_qpdf_enqueue_refs`
  - `renumber_qpdf_refs_in_place`
  - byte-identical Disable/Preserve output.

- [ ] **Step 1: Add the classic matrix fixtures and qpdf goldens**

Add deterministic inline Python blocks to phase 1 of
`tests/golden/regenerate.sh`, following the existing
`missing-trailer-info.pdf` block. The blocks must use only literal object
bodies, computed byte offsets, and Python's standard-library `zlib`; they must
not invoke flpdf or qpdf to construct a source fixture.

Build `null-visible-matrix.pdf` as a classic-xref PDF with one Catalog, one
Pages node, one Page, one content stream, a live null object, a one-hop holder
to that null, and a free xref row. Its Catalog must contain:

```pdf
/DirectNull null
/Zero 0 0 R
/Missing 99 0 R
/Free 8 0 R
/RealNull 5 0 R
/Holder 6 0 R
/Nested << /Drop 5 0 R /KeepArray [ 0 0 R 99 0 R 8 0 R 5 0 R 6 0 R ] >>
/KeepArray [ 0 0 R 99 0 R 8 0 R 5 0 R 6 0 R ]
/ArrayDict [ << /Drop 5 0 R /Keep 6 0 R >> ]
```

The content stream dictionary must contain `/Drop 5 0 R` beside a direct
`/Length`. Object 5 is `null`, object 6 is `5 0 R`, and xref object 8 is free.

Create `null-visible-cycle.pdf` separately with objects `6 -> 7 -> 6`; put
`/Cycle 6 0 R` in the Catalog so qpdf's warning/cache behavior is isolated.

Create `null-visible-matrix-objstm.pdf` by packing the matrix's non-stream
objects into a valid source ObjStm while keeping the same Catalog edges. Reuse
the pair-table and xref-stream construction pattern from
`object_streams_writer_tests.rs`; do not generate this source by first passing
it through qpdf because qpdf would already remove the edges under test.

For every classic-xref source block, use this exact offset writer:

```python
def write_classic(path, objects, size, free_numbers=()):
    body = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    for number, object_body in objects:
        offsets[number] = len(body)
        body += b"%d 0 obj\n" % number + object_body + b"\nendobj\n"
    xref = len(body)
    body += b"xref\n0 %d\n" % (max(max(offsets), max(free_numbers, default=0)) + 1)
    body += b"0000000000 65535 f \n"
    for number in range(1, max(max(offsets), max(free_numbers, default=0)) + 1):
        if number in offsets:
            body += b"%010d 00000 n \n" % offsets[number]
        else:
            body += b"0000000000 00000 f \n"
    body += (
        b"trailer\n<< /Size %d /Root 1 0 R >>\n"
        b"startxref\n%d\n%%%%EOF\n"
    ) % (size, xref)
    open(path, "wb").write(body)
```

Call it for the matrix with objects 1 through 6, `size=100`, and
`free_numbers=(7, 8)`; object 7 is an ordinary unused free row and object 8 is
the explicitly tested free reference. Call it for the cycle fixture with
objects 1 through 7, `size=8`, and no free rows. The matrix object bodies are:

```python
matrix_objects = [
    (1, b"<< /Type /Catalog /Pages 2 0 R /DirectNull null /Zero 0 0 R "
        b"/Missing 99 0 R /Free 8 0 R /RealNull 5 0 R /Holder 6 0 R "
        b"/Nested << /Drop 5 0 R /KeepArray [0 0 R 99 0 R 8 0 R 5 0 R 6 0 R] >> "
        b"/KeepArray [0 0 R 99 0 R 8 0 R 5 0 R 6 0 R] "
        b"/ArrayDict [<< /Drop 5 0 R /Keep 6 0 R >>] >>"),
    (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
    (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Resources << >> /Contents 4 0 R >>"),
    (4, b"<< /Length 0 /Drop 5 0 R >>\nstream\n\nendstream"),
    (5, b"null"),
    (6, b"5 0 R"),
]
```

The cycle block copies objects 1 through 4, omits the matrix-only Catalog
keys, adds `/Cycle 6 0 R`, and uses `(5, b"null"), (6, b"7 0 R"),
(7, b"6 0 R")`. The ObjStm block uses the same `matrix_objects`, packs objects
1, 2, 3, 5, and 6 in that order into object 9, leaves stream object 4 plain,
and writes object 10 as an xref stream with `/W [1 4 2]`,
`/Index [0 11]`, `/Size 100`, and `/Root 1 0 R`. Its type-2 rows point to
container 9 with member indices 0 through 4; rows 7 and 8 are free. This is the
same binary layout implemented by `append_xref_entry` in
`object_streams_writer_tests.rs`, with four-byte field 1 and two-byte field 2.

Append exact oracle commands to `tests/golden/regenerate.sh`:

```bash
qpdf --object-streams=disable --static-id --warning-exit-0 \
    "$FIX/null-visible-matrix.pdf" \
    "$REF/null-visible-matrix/disable.pdf"
qpdf --object-streams=preserve --static-id --warning-exit-0 \
    "$FIX/null-visible-matrix-objstm.pdf" \
    "$REF/null-visible-matrix-objstm/preserve.pdf"
qpdf --object-streams=disable --static-id --warning-exit-0 \
    "$FIX/null-visible-cycle.pdf" \
    "$REF/null-visible-cycle/disable.pdf"
qpdf --check --warning-exit-0 "$REF/null-visible-matrix/disable.pdf"
qpdf --check --warning-exit-0 "$REF/null-visible-matrix-objstm/preserve.pdf"
qpdf --check --warning-exit-0 "$REF/null-visible-cycle/disable.pdf"
```

Run those commands and commit only qpdf-produced reference files.

- [ ] **Step 2: Write strict Disable/Preserve tests**

Create `cmp_null_visibility_tests.rs`, gated by
`#![cfg(feature = "qpdf-zlib-compat")]`. Implement:

```rust
fn rewrite_mode(fixture: &str, mode: ObjectStreamMode) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/compat")
        .join(fixture);
    let mut pdf = Pdf::open(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut options = WriteOptions::default();
    options.full_rewrite = true;
    options.object_streams = mode;
    options.static_id = true;
    options.newline_before_endstream = NewlineBeforeEndstream::Never;
    let mut out = Vec::new();
    write_pdf_with_options(&mut pdf, &mut out, &options).unwrap();
    out
}

#[test]
fn disable_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix.pdf", ObjectStreamMode::Disable),
        "null-visible-matrix/disable.pdf",
    );
}

#[test]
fn preserve_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode(
            "null-visible-matrix-objstm.pdf",
            ObjectStreamMode::Preserve,
        ),
        "null-visible-matrix-objstm/preserve.pdf",
    );
}

#[test]
fn disable_null_visibility_cycle_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-cycle.pdf", ObjectStreamMode::Disable),
        "null-visible-cycle/disable.pdf",
    );
}
```

Copy the complete first-difference diagnostic style from
`cmp_diff_zero_tests.rs`; do not reduce failures to a bare `assert_eq!`.

- [ ] **Step 3: Run strict tests and verify RED**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests -- --nocapture
```

Expected: the matrix Disable/Preserve tests and cycle Disable test fail at
dictionary-key/object-number/`/Size` bytes; record the first offsets in the
task report.

- [ ] **Step 4: Add qpdf direct-object collection without changing BFS**

In `rewrite_renumber.rs`, keep the existing generic `collect_refs` for QDF and
excluded paths. Add:

```rust
fn collect_qpdf_enqueue_refs<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    obj: &Object,
    depth: usize,
    skip_length: bool,
    found: &mut Vec<ObjectRef>,
) -> crate::Result<()> {
    if depth > MAX_INLINE_DEPTH {
        return Err(Error::Unsupported(
            "plain rewrite: inline object nesting exceeds MAX_INLINE_DEPTH during \
             qpdf enqueue collection"
                .to_string(),
        ));
    }
    match obj {
        Object::Reference(reference) => {
            if reference.number != 0 {
                found.push(*reference);
            }
        }
        Object::Array(items) => {
            for item in items {
                collect_qpdf_enqueue_refs(pdf, item, depth + 1, skip_length, found)?;
            }
        }
        Object::Dictionary(dict) => {
            let entries = crate::qpdf_null::snapshot_entries(dict, false);
            for (_, value) in crate::qpdf_null::visible_entries(pdf, entries)? {
                collect_qpdf_enqueue_refs(pdf, &value, depth + 1, skip_length, found)?;
            }
        }
        Object::Stream(stream) => {
            let entries = crate::qpdf_null::snapshot_entries(&stream.dict, skip_length);
            for (_, value) in crate::qpdf_null::visible_entries(pdf, entries)? {
                collect_qpdf_enqueue_refs(pdf, &value, depth + 1, skip_length, found)?;
            }
        }
        _ => {}
    }
    Ok(())
}
```

Use an owned `Vec<ObjectRef>` per resolved queue object, then call the existing
`enqueue` in that vector's order. Do not change `VecDeque`, `old_to_new`, or
first-encounter numbering.

- [ ] **Step 5: Add resolver-aware qpdf reference rewriting**

Keep `renumber_refs_in_place` unchanged for QDF/encryption. Add:

```rust
pub(crate) fn renumber_qpdf_refs_in_place<R: Read + Seek, M: NewNumberLookup>(
    pdf: &mut Pdf<R>,
    obj: &mut Object,
    map: &M,
) -> crate::Result<()> {
    rewrite_qpdf(pdf, obj, 0, map)
}
```

`rewrite_qpdf` must:

- replace object-0 array references with `Object::Null`;
- map every other surviving array reference, including missing/free/REAL-null
  refs that received a number;
- rebuild dictionaries and stream dictionaries from
  `qpdf_null::visible_entries`;
- preserve the existing special directization of an unmapped stream `/Length`;
- return the existing unmapped-reference error for a surviving non-null edge.

The dictionary arm must follow this shape:

```rust
let entries = crate::qpdf_null::snapshot_entries(dict, false);
let entries = crate::qpdf_null::visible_entries(pdf, entries)?;
let mut rewritten = Dictionary::new();
for (key, mut value) in entries {
    rewrite_qpdf(pdf, &mut value, depth + 1, map)?;
    rewritten.insert(key, value);
}
*dict = rewritten;
```

- [ ] **Step 6: Route only plain non-QDF Disable/Preserve through the new path**

In `writer.rs`, use `renumber_qpdf_refs_in_place` when all are true:

```rust
!options.qdf && options.encrypt.is_none() && options.copy_encryption.is_none()
```

Apply the same predicate to trailer remapping. Build an owned trailer
`Object::Dictionary`, run the qpdf-aware rewrite, then extract the dictionary.
Leave QDF and encryption on the existing `renumber_refs_in_place` and
`remap_trailer_refs` paths.

- [ ] **Step 7: Add unit guards for BFS and source immutability**

In `rewrite_renumber.rs`, add tests asserting:

```rust
assert_eq!(
    map.new_for_original(ObjectRef::new(5, 0)),
    None,
    "dict-only REAL-null must not receive a number"
);
assert!(
    map.new_for_original(ObjectRef::new(99, 0)).is_some(),
    "the same missing ref reached from an array must receive a number"
);
assert_eq!(
    pdf.resolve(root).unwrap(),
    original_root,
    "visibility analysis must not mutate the source graph"
);
```

Also assert the exact `map.pairs()` source order against qpdf's output object
order for the matrix fixture.

- [ ] **Step 8: Run focused and strict tests**

Run:

```bash
cargo test -p flpdf rewrite_renumber::tests -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
cargo test -p flpdf --test object_streams_writer_tests
```

Expected: all pass; the strict matrix reports no differing byte.

- [ ] **Step 9: Commit and close the bottom issue**

```bash
git add crates/flpdf/src/rewrite_renumber.rs crates/flpdf/src/writer.rs \
  crates/flpdf/tests/cmp_null_visibility_tests.rs \
  tests/fixtures/compat/null-visible-matrix.pdf \
  tests/fixtures/compat/null-visible-matrix-objstm.pdf \
  tests/fixtures/compat/null-visible-cycle.pdf \
  tests/golden/regenerate.sh tests/golden/references/null-visible-matrix \
  tests/golden/references/null-visible-matrix-objstm \
  tests/golden/references/null-visible-cycle
git commit -m "fix(writer): port qpdf null-aware standard enqueue"
bd close flpdf-9hc.40 --reason \
  "qpdf 11.9.0 null-aware getKeys semantics now drive plain Disable/Preserve BFS, reachability, and serialization with strict byte parity"
bd dolt push
```

---

### Task 3: Generate DFS, ObjStm Membership, and flpdf-v58c

**Issue/branch:** `flpdf-v58c` on
`fix/flpdf-v58c-generate-null-walk`, based on
`refactor/flpdf-v58c-qpdf-null-walk`

**Files:**
- Modify: `crates/flpdf/src/writer/object_streams.rs`
- Modify: `crates/flpdf/src/rewrite_renumber.rs`
- Modify: `crates/flpdf/src/writer.rs`
- Modify: `crates/flpdf/tests/cmp_null_visibility_tests.rs`
- Create: `tests/fixtures/compat/null-visible-split-boundary.pdf`
- Modify: `tests/golden/regenerate.sh`
- Create: `tests/golden/references/null-visible-matrix/generate.pdf`
- Create: `tests/golden/references/null-visible-split-boundary/generate.pdf`

**Interfaces:**
- Consumes: Task 1's qpdf-null helpers and Task 2's qpdf-aware BFS/rewriter.
- Produces: exact `getCompressibleObjGens` null visibility, Generate ObjStm
  grouping, and byte-identical non-linearized Generate output.

- [ ] **Step 1: Create and claim the middle stack branch**

```bash
git switch -c fix/flpdf-v58c-generate-null-walk
bd update flpdf-v58c --claim
```

Verify:

```bash
git merge-base --is-ancestor refactor/flpdf-v58c-qpdf-null-walk HEAD
bd show flpdf-v58c
```

Expected: ancestor check exits 0; `flpdf-v58c` is `IN_PROGRESS` and unblocked.

- [ ] **Step 2: Add Generate oracle outputs and failing tests**

Append:

```bash
qpdf --object-streams=generate --static-id --warning-exit-0 \
    "$FIX/null-visible-matrix.pdf" \
    "$REF/null-visible-matrix/generate.pdf"
qpdf --object-streams=generate --static-id --warning-exit-0 \
    "$FIX/null-visible-split-boundary.pdf" \
    "$REF/null-visible-split-boundary/generate.pdf"
qpdf --check --warning-exit-0 "$REF/null-visible-matrix/generate.pdf"
qpdf --check --warning-exit-0 "$REF/null-visible-split-boundary/generate.pdf"
```

Generate the boundary fixture in phase 1 with the same `write_classic` helper.
It contains a Catalog, Pages, Page, object 4 whose body is `null`, and 102 live
dictionary objects numbered 5 through 106. Put references to 5 through 106 in
a Catalog `/Candidates` array in ascending order, add `/Drop 4 0 R` to the
Catalog dictionary, and append `4 0 R` as the final array element. This gives
qpdf exactly 106 compressible candidates in source DFS order: Catalog, Pages,
Page, objects 5 through 106, and the array-reached REAL-null object 4. The
dict-only visit to object 4 is invisible and therefore does not consume the
first-visit slot. The qpdf oracle must contain two evenly split ObjStms with
`/N 53` and `/N 53`.

Add tests:

```rust
#[test]
fn generate_null_visibility_matrix_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode("null-visible-matrix.pdf", ObjectStreamMode::Generate),
        "null-visible-matrix/generate.pdf",
    );
}

#[test]
fn generate_null_visibility_split_boundary_is_byte_identical_to_qpdf() {
    assert_golden(
        &rewrite_mode(
            "null-visible-split-boundary.pdf",
            ObjectStreamMode::Generate,
        ),
        "null-visible-split-boundary/generate.pdf",
    );
}
```

- [ ] **Step 3: Run Generate strict tests and verify RED**

```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests generate_ -- --nocapture
```

Expected: both Generate tests fail before the DFS/Generate-BFS change.

- [ ] **Step 4: Port qpdf-visible dictionary children into the existing DFS**

Change `push_children`/`push_dict_children` to return `Result` and accept
`&mut Pdf`. Preserve the existing reverse push:

```rust
fn push_dict_children<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    dict: &Dictionary,
    stack: &mut Vec<Object>,
    is_stream: bool,
) -> crate::Result<()> {
    let entries = crate::qpdf_null::snapshot_entries(dict, is_stream);
    let entries = crate::qpdf_null::visible_entries(pdf, entries)?;
    for (_, value) in entries.into_iter().rev() {
        stack.push(value);
    }
    Ok(())
}
```

In the indirect-reference arm of `compressible_objgens`:

- skip object 0;
- retain first-visit-by-object-number behavior;
- resolve the object;
- include an array-reached missing/free/REAL-null reference as a candidate;
- retain stream/signature/encryption exclusions;
- remove the old `live.contains(&r)` eligibility approximation.

Do not change stack LIFO order or `even_split_into_streams`.

- [ ] **Step 5: Make GenerateRenumber use the Task 2 BFS**

Replace its calls to generic `collect_refs` with
`collect_qpdf_enqueue_refs`. Preserve:

- `member_to_group`;
- ascending-source `groups_sorted`;
- container reservation;
- `old_to_new`;
- `VecDeque` processing.

No new Generate-specific null walk is allowed.

- [ ] **Step 6: Use qpdf-aware rewriting in the dedicated Generate emitter**

At both plain-body and ObjStm-member emission sites in `write_pdf_generate`,
replace:

```rust
renumber_refs_in_place(&mut object, &renumber)?;
```

with:

```rust
renumber_qpdf_refs_in_place(pdf, &mut object, &renumber)?;
```

Do the same for each resolved ObjStm member. Keep stream re-encoding,
container dictionary order, xref encoding, and trailer-ID logic unchanged.

- [ ] **Step 7: Add exact DFS/grouping unit guards**

In `object_streams.rs`, assert the candidate order for the matrix fixture,
including the array-reached null refs and excluding dict-only null refs:

```rust
assert!(!eligible.contains(&dict_only_real_null));
assert!(eligible.contains(&array_real_null));
assert!(eligible.contains(&array_missing));
```

For the boundary fixture:

```rust
let groups = even_split_into_streams(&eligible);
assert_eq!(eligible.len(), 106);
assert_eq!(groups.len(), 2);
assert_eq!(
    groups.iter().map(Vec::len).collect::<Vec<_>>(),
    vec![53, 53]
);
```

Use qpdf `--show-xref` and `--show-object=1 --filtered-stream-data` on the
committed boundary oracle to confirm the two literal expectations before
writing the assertions; do not infer them from flpdf output.

- [ ] **Step 8: Run focused and strict Generate suites**

```bash
cargo test -p flpdf writer::object_streams::tests -- --nocapture
cargo test -p flpdf rewrite_renumber::tests -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_generate_objstm_tests
```

Expected: all pass.

- [ ] **Step 9: Commit and close flpdf-v58c**

```bash
git add crates/flpdf/src/writer/object_streams.rs \
  crates/flpdf/src/rewrite_renumber.rs crates/flpdf/src/writer.rs \
  crates/flpdf/tests/cmp_null_visibility_tests.rs \
  tests/fixtures/compat/null-visible-split-boundary.pdf \
  tests/golden/regenerate.sh \
  tests/golden/references/null-visible-matrix/generate.pdf \
  tests/golden/references/null-visible-split-boundary
git commit -m "fix(writer): match qpdf null-aware generate traversal"
bd close flpdf-v58c --reason \
  "qpdf 11.9.0 getKeys visibility now drives Generate DFS, BFS numbering, ObjStm membership, and serialization with strict byte parity"
bd dolt push
```

---

### Task 4: Linearization Convergence

**Issue/branch:** `flpdf-9hc.41` on
`refactor/flpdf-9hc-41-linearize-null-walk`, based on
`fix/flpdf-v58c-generate-null-walk`

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs`
- Modify: `crates/flpdf/src/linearization/plan.rs`
- Modify: `crates/flpdf/src/linearization/writer.rs`
- Modify: `crates/flpdf/tests/cmp_null_visibility_tests.rs`
- Modify: `tests/golden/regenerate.sh`
- Create: `tests/golden/references/null-visible-matrix/linearize.pdf`
- Create: `tests/golden/references/null-visible-matrix/linearize-objstm.pdf`
- Create: `tests/golden/references/null-visible-matrix-objstm/linearize-objstm-preserve.pdf`

**Interfaces:**
- Consumes: the exact qpdf-null predicate from Task 1.
- Produces: one null semantic shared by non-linearized and linearized writers
  without changing page/user/section order.

- [ ] **Step 1: Create and claim the top stack branch**

```bash
git switch -c refactor/flpdf-9hc-41-linearize-null-walk
bd update flpdf-9hc.41 --claim
```

Verify:

```bash
git merge-base --is-ancestor fix/flpdf-v58c-generate-null-walk HEAD
bd show flpdf-9hc.41
```

Expected: ancestor check exits 0 and the issue is `IN_PROGRESS`.

- [ ] **Step 2: Add linearization oracle outputs and failing REAL-null tests**

Append:

```bash
qpdf --linearize --object-streams=disable --deterministic-id --warning-exit-0 \
    "$FIX/null-visible-matrix.pdf" \
    "$REF/null-visible-matrix/linearize.pdf"
qpdf --linearize --object-streams=generate --deterministic-id --warning-exit-0 \
    "$FIX/null-visible-matrix.pdf" \
    "$REF/null-visible-matrix/linearize-objstm.pdf"
qpdf --linearize --object-streams=preserve --deterministic-id --warning-exit-0 \
    "$FIX/null-visible-matrix-objstm.pdf" \
    "$REF/null-visible-matrix-objstm/linearize-objstm-preserve.pdf"
qpdf --check-linearization "$REF/null-visible-matrix/linearize.pdf"
qpdf --check-linearization "$REF/null-visible-matrix/linearize-objstm.pdf"
qpdf --check-linearization \
    "$REF/null-visible-matrix-objstm/linearize-objstm-preserve.pdf"
```

Extend `cmp_null_visibility_tests.rs` with three tests using the public
linearization API and `deterministic_id = true`. Copy the option construction
from `cmp_linearize_objstm_tests.rs`, including Preserve's distinct golden
name.

- [ ] **Step 3: Run the new linearized matrix and verify RED or proven no-op**

```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests linearize_ -- --nocapture
```

Expected: any current REAL-null gap fails. If all tests already pass, retain
them as convergence guards and continue with the refactor; the next step must
produce no golden changes.

- [ ] **Step 4: Replace `!live` in resurrectable traversal**

Refactor `resurrectable_null_refs` and `walk_surviving` to consult
`qpdf_null::value_is_null` for dictionary visibility. Preserve its position
rule:

```rust
if crate::qpdf_null::value_is_null(pdf, &value)? {
    if in_array {
        if let Object::Reference(reference) = value {
            if reference.number > 0 {
                result.insert(reference);
            }
        }
    }
    continue;
}
```

Live refs whose body is `Object::Null` are now classified the same as
missing/free refs. Preserve visited order and array first-edge behavior.

- [ ] **Step 5: Replace linearization writer local null approximations**

Delete `is_null_resolving_ref` and `is_null_resolving_value` from
`linearization/writer.rs`. Pass `&mut Pdf` into the recursive renumber helper
and rebuild dictionaries from qpdf-visible snapshots exactly as Task 2 does.

Do not change:

- `RenumberMap`;
- part ordering;
- hint dictionaries;
- stream-length replacement;
- encryption hooks;
- xref/hint two-pass layout.

- [ ] **Step 6: Verify all existing linearization bytes**

```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat \
  --test linearize_objstm_generate_tests
```

Expected: all pass. Do not re-bless any pre-existing golden unless a separate
qpdf 11.9.0 command proves the old golden wrong.

- [ ] **Step 7: Commit and close the top issue**

```bash
git add crates/flpdf/src/rewrite_renumber.rs \
  crates/flpdf/src/linearization/plan.rs \
  crates/flpdf/src/linearization/writer.rs \
  crates/flpdf/tests/cmp_null_visibility_tests.rs \
  tests/golden/regenerate.sh \
  tests/golden/references/null-visible-matrix/linearize.pdf \
  tests/golden/references/null-visible-matrix/linearize-objstm.pdf \
  tests/golden/references/null-visible-matrix-objstm/linearize-objstm-preserve.pdf
git commit -m "refactor(linearize): share qpdf null-aware traversal"
bd close flpdf-9hc.41 --reason \
  "linearized Disable/Preserve/Generate now share the qpdf 11.9.0 null predicate while preserving page, user, section, and byte ordering"
bd dolt push
```

---

### Task 5: Per-Layer Quality and Coverage Gates

**Files:**
- No product-file changes expected.
- Modify only uncovered test files when a legitimate executable changed line
  lacks coverage.

**Interfaces:**
- Consumes: each committed stack-layer `HEAD`.
- Produces: independently mergeable green layers with 100% patch coverage.

- [ ] **Step 1: Verify the bottom layer from its committed HEAD**

```bash
git switch refactor/flpdf-v58c-qpdf-null-walk
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_diff_zero_tests
scripts/patch-coverage.sh --base main
```

Expected: every command exits 0 and patch coverage reports 100% for
`crates/flpdf/src`.

- [ ] **Step 2: Verify the middle layer against its parent**

```bash
git switch fix/flpdf-v58c-generate-null-walk
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_generate_objstm_tests
scripts/patch-coverage.sh --base refactor/flpdf-v58c-qpdf-null-walk
```

Expected: every command exits 0 and middle-layer changed-line coverage is 100%.

- [ ] **Step 3: Verify the top layer against its parent**

```bash
git switch refactor/flpdf-9hc-41-linearize-null-walk
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_null_visibility_tests
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_tests
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat \
  --test linearize_objstm_generate_tests
scripts/patch-coverage.sh --base fix/flpdf-v58c-generate-null-walk
```

Expected: every command exits 0 and top-layer changed-line coverage is 100%.

- [ ] **Step 4: Re-run qpdf validation over every new oracle**

```bash
find tests/golden/references/null-visible-* -type f -name '*.pdf' -print0 |
  xargs -0 -n1 qpdf --check --warning-exit-0
qpdf --check-linearization \
  tests/golden/references/null-visible-matrix/linearize.pdf
qpdf --check-linearization \
  tests/golden/references/null-visible-matrix/linearize-objstm.pdf
qpdf --check-linearization \
  tests/golden/references/null-visible-matrix-objstm/linearize-objstm-preserve.pdf
```

Expected: all files pass; linearized outputs report no linearization errors.

---

### Task 6: Publish the Dependent Draft PR Stack

**Files:**
- No source changes.

**Interfaces:**
- Consumes: three clean, verified, committed branches.
- Produces: three pushed branches and three draft PRs with dependent bases.

- [ ] **Step 1: Push every branch and Beads state**

State before the push must be reported to the user: three branches are being
published; no force-push is needed.

```bash
git push -u origin refactor/flpdf-v58c-qpdf-null-walk
git push -u origin fix/flpdf-v58c-generate-null-walk
git push -u origin refactor/flpdf-9hc-41-linearize-null-walk
bd dolt push
```

Expected: all remote branches match local `HEAD`.

- [ ] **Step 2: Create the bottom draft PR**

Create `/tmp/flpdf-v58c-layer1-pr.md` before invoking `gh`. Its exact headings
are `Summary`, `qpdf 11.9.0 mapping`, `Compatibility matrix`, and `Test plan`.
Under them, record the `QPDFWriter::enqueueObject` and
`QPDF_Dictionary::getKeys` source functions, the three strict Disable/Preserve
goldens, every Task 5 bottom-layer command, the verbatim patch-coverage summary,
and `Tracked by flpdf-9hc.40`. Do not include an unfilled template field.

```bash
gh pr create --draft \
  --base main \
  --head refactor/flpdf-v58c-qpdf-null-walk \
  --title "refactor(writer): port qpdf null-aware standard enqueue" \
  --body-file /tmp/flpdf-v58c-layer1-pr.md
```

The body must contain Summary, qpdf 11.9.0 algorithm/source mapping, Test plan,
Compat matrix, `flpdf-9hc.40`, and the exact patch-coverage result.

- [ ] **Step 3: Create the middle draft PR**

Create `/tmp/flpdf-v58c-layer2-pr.md` first, with headings `Depends on`,
`Summary`, `qpdf 11.9.0 mapping`, `Boundary result`, and `Test plan`. Insert the
actual bottom PR URL, name `QPDF::getCompressibleObjGens` and
`QPDFWriter::enqueueObject`, record the oracle-confirmed `106 -> [53, 53]`
split, paste the middle-layer Task 5 results and patch-coverage summary, and end
with `Tracked by flpdf-v58c`. Do not include an unfilled template field.

```bash
gh pr create --draft \
  --base refactor/flpdf-v58c-qpdf-null-walk \
  --head fix/flpdf-v58c-generate-null-walk \
  --title "fix(writer): match qpdf null-aware generate traversal" \
  --body-file /tmp/flpdf-v58c-layer2-pr.md
```

The body must link the bottom PR, explain DFS versus BFS ordering, include the
100-member boundary result, and identify `flpdf-v58c`.

- [ ] **Step 4: Create the top draft PR**

Create `/tmp/flpdf-v58c-layer3-pr.md` first, with headings `Depends on`,
`Summary`, `Ordering contract`, and `Test plan`. Insert both lower PR URLs,
state that page/user/section and hint ordering did not change, paste every
top-layer Task 5 result and its patch-coverage summary, and end with
`Tracked by flpdf-9hc.41`. Do not include an unfilled template field.

```bash
gh pr create --draft \
  --base fix/flpdf-v58c-generate-null-walk \
  --head refactor/flpdf-9hc-41-linearize-null-walk \
  --title "refactor(linearize): share qpdf null-aware traversal" \
  --body-file /tmp/flpdf-v58c-layer3-pr.md
```

The body must link both lower PRs, state that linearization ordering is
unchanged, list strict linearization gates, and identify `flpdf-9hc.41`.

- [ ] **Step 5: Verify published state**

```bash
gh pr list --state open \
  --json number,title,baseRefName,headRefName,isDraft,url
git status --short
git rev-parse HEAD
git rev-parse '@{upstream}'
bd show flpdf-9hc.40
bd show flpdf-v58c
bd show flpdf-9hc.41
```

Expected: all three PRs are draft with the intended dependent bases, the
current worktree is clean, local/remote heads match, and all three implemented
issues are closed.
