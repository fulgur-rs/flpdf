# First-page ObjStm Container Ordering Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make linearized Generate and Preserve output order multiple first-page ObjStm containers byte-identically to qpdf 11.9.0: private, then shared, then first-page outlines.

**Architecture:** Refine the canonical container route so it retains qpdf's first-page private/shared/outline subsection. Reuse one thumbnail-user helper in both the classic partition and ObjStm router, then use stable route buckets in Generate and Preserve so their existing container-number order is preserved within each qpdf subsection.

**Tech Stack:** Rust 2021, existing `LinearizationPlan` and object-stream writer, Python 3 fixture generator, qpdf 11.9.0 as the byte oracle, `cargo test`, and `scripts/patch-coverage.sh`.

## Global Constraints

- qpdf 11.9.0 source and observed output are the behavior oracle.
- Cover both linearized `ObjectStreamMode::Generate` and `ObjectStreamMode::Preserve`.
- Do not change non-linearized object streams, compression, eligibility, container membership, or second-half ordering.
- Preserve source ObjStm membership and member-index order; do not split or repack a preserved container.
- Generate retains the existing global even split and source-object-number member order.
- Every changed executable line under `crates/flpdf/src` must have 100% patch coverage from the final committed `HEAD`.
- Goldens require the `qpdf-zlib-compat` feature for strict byte identity.

---

## File Map

- Create `docs/plans/tools/gen_firstpage_objstm_container_order.py`: emit the small authored classic fixture with 110 first-page Font dictionaries and one Catalog sharing edge.
- Create `tests/fixtures/compat/objstm-lin-firstpage-private-before-shared.pdf`: generated classic input for Generate mode.
- Create `tests/fixtures/compat/objstm-lin-firstpage-private-before-shared-bearing.pdf`: qpdf-derived ObjStm-bearing input for Preserve mode.
- Create `tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf`: qpdf 11.9.0 Generate oracle.
- Create `tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf`: qpdf 11.9.0 Preserve oracle.
- Modify `tests/golden/regenerate.sh`: deterministically rebuild both fixtures/goldens and validate them.
- Modify `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`: add structural and strict byte gates for Generate and Preserve.
- Modify `crates/flpdf/src/linearization/plan.rs`: share thumbnail-user calculation, retain exact first-page routes, and bucket both modes by those routes.
- Modify `crates/flpdf/src/linearization/writer.rs`: recognize all refined first-half route variants as unreachable in second-half placement.

---

### Task 1: Reproduce and Pin the qpdf Oracle

**Files:**
- Create: `docs/plans/tools/gen_firstpage_objstm_container_order.py`
- Create: `tests/fixtures/compat/objstm-lin-firstpage-private-before-shared.pdf`
- Create: `tests/fixtures/compat/objstm-lin-firstpage-private-before-shared-bearing.pdf`
- Create: `tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf`
- Create: `tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf`
- Modify: `tests/golden/regenerate.sh`

**Interfaces:**
- Produces: a classic fixture for `flpdf_linearized_objstm` and an ObjStm-bearing fixture for `flpdf_linearized_objstm_preserve`.
- Produces: qpdf 11.9.0 goldens named exactly as the existing `golden` and `golden_preserve` helpers expect.

- [ ] **Step 1: Write the deterministic authored-fixture generator**

Create `docs/plans/tools/gen_firstpage_objstm_container_order.py`:

```python
#!/usr/bin/env python3
"""Generate a first-page ObjStm private-before-shared ordering fixture."""

from pathlib import Path
import sys


def indirect(number: int, body: bytes) -> bytes:
    return f"{number} 0 obj\n".encode() + body + b"\nendobj\n"


def main() -> None:
    output = Path(sys.argv[1])
    first_font = 4
    font_count = 110
    content_number = first_font + font_count

    font_entries = b" ".join(
        f"/F{i:03d} {first_font + i} 0 R".encode() for i in range(font_count)
    )
    objects = [
        indirect(
            1,
            b"<< /Type /Catalog /Pages 2 0 R /Ref2 4 0 R >>",
        ),
        indirect(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        indirect(
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            + b"/Resources << /Font << "
            + font_entries
            + b" >> >> /Contents "
            + f"{content_number} 0 R".encode()
            + b" >>",
        ),
    ]
    objects.extend(
        indirect(
            first_font + i,
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        )
        for i in range(font_count)
    )
    content = b"BT /F000 12 Tf 72 720 Td (first page) Tj ET\n"
    objects.append(
        indirect(
            content_number,
            f"<< /Length {len(content)} >>\nstream\n".encode()
            + content
            + b"endstream",
        )
    )

    pdf = bytearray(b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for obj in objects:
        offsets.append(len(pdf))
        pdf.extend(obj)
    xref = len(pdf)
    pdf.extend(f"xref\n0 {len(offsets)}\n".encode())
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets[1:]:
        pdf.extend(f"{offset:010d} 00000 n \n".encode())
    pdf.extend(
        b"trailer\n"
        + f"<< /Size {len(offsets)} /Root 1 0 R >>\n".encode()
        + f"startxref\n{xref}\n%%EOF\n".encode()
    )
    output.write_bytes(pdf)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Generate the classic fixture and confirm its shape**

Run:

```bash
python3 docs/plans/tools/gen_firstpage_objstm_container_order.py \
  tests/fixtures/compat/objstm-lin-firstpage-private-before-shared.pdf
qpdf --check tests/fixtures/compat/objstm-lin-firstpage-private-before-shared.pdf
```

Expected: `qpdf --check` exits zero. The fixture has one page, 110 Font
dictionaries, and `/Catalog /Ref2 4 0 R`; object 4 is both first-page-reached
and document-reached.

- [ ] **Step 3: Add deterministic regeneration commands**

In Phase 1 of `tests/golden/regenerate.sh`, add:

```bash
python3 "$ROOT/docs/plans/tools/gen_firstpage_objstm_container_order.py" \
    "$FIX/objstm-lin-firstpage-private-before-shared.pdf"

qpdf --object-streams=generate --deterministic-id --warning-exit-0 \
    "$FIX/objstm-lin-firstpage-private-before-shared.pdf" \
    "$FIX/objstm-lin-firstpage-private-before-shared-bearing.pdf"
```

In the linearized ObjStm golden section, add:

```bash
mkdir -p "$REF/objstm-lin-firstpage-private-before-shared"
qpdf --linearize --object-streams=generate --deterministic-id --warning-exit-0 \
    "$FIX/objstm-lin-firstpage-private-before-shared.pdf" \
    "$REF/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf"

mkdir -p "$REF/objstm-lin-firstpage-private-before-shared-bearing"
qpdf --linearize --object-streams=preserve --deterministic-id --warning-exit-0 \
    "$FIX/objstm-lin-firstpage-private-before-shared-bearing.pdf" \
    "$REF/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf"

qpdf --check-linearization \
    "$REF/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf"
qpdf --check-linearization \
    "$REF/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf"
```

- [ ] **Step 4: Regenerate and inspect the qpdf container order**

Run:

```bash
bash tests/golden/regenerate.sh
qpdf --show-object=all \
  tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf \
  | rg '/Type /ObjStm|/N '
qpdf --show-object=all \
  tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf \
  | rg '/Type /ObjStm|/N '
```

Expected: regeneration exits zero; both outputs contain at least two
first-page ObjStms. The container containing only private Font members is
numbered/emitted before the container containing object 4 and therefore the
document-sharing signal.

- [ ] **Step 5: Verify deterministic fixture and golden regeneration**

Run:

```bash
git diff -- tests/fixtures/compat/objstm-lin-firstpage-private-before-shared.pdf \
  tests/fixtures/compat/objstm-lin-firstpage-private-before-shared-bearing.pdf \
  tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf \
  tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf
bash tests/golden/regenerate.sh
git diff --exit-code -- tests/fixtures/compat/objstm-lin-firstpage-private-before-shared.pdf \
  tests/fixtures/compat/objstm-lin-firstpage-private-before-shared-bearing.pdf \
  tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf \
  tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf
```

Expected: the second command leaves the four generated files byte-unchanged.

- [ ] **Step 6: Commit the oracle corpus**

```bash
git add docs/plans/tools/gen_firstpage_objstm_container_order.py \
  tests/golden/regenerate.sh \
  tests/fixtures/compat/objstm-lin-firstpage-private-before-shared.pdf \
  tests/fixtures/compat/objstm-lin-firstpage-private-before-shared-bearing.pdf \
  tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf \
  tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf
git commit -m "test(linearize): add first-page ObjStm ordering oracle"
```

---

### Task 2: Add RED Generate and Preserve Parity Tests

**Files:**
- Modify: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Interfaces:**
- Consumes: `flpdf_linearized_objstm`, `flpdf_linearized_objstm_preserve`, `golden`, `golden_preserve`, `mask_id1`, and `report`.
- Produces: four regression tests that fail on the current collapsed `FirstPage` route.

- [ ] **Step 1: Add structural and strict Generate tests**

Append near the existing `catalog_firstpage_shared_two_page` tests:

```rust
// flpdf-19ac: qpdf classifies the generated ObjStm container union, not each
// member independently. The earlier even-split container contains obj 4,
// which is first-page-shared through Catalog /Ref2. The later container is
// first-page-private, so qpdf emits the later private container first.
#[test]
fn firstpage_private_container_precedes_shared_generate_structurally() {
    assert_structural(
        "objstm-lin-firstpage-private-before-shared.pdf",
        "objstm-lin-firstpage-private-before-shared",
    );
}

#[test]
fn firstpage_private_container_precedes_shared_generate_byte_identical_to_qpdf() {
    assert_strict(
        "objstm-lin-firstpage-private-before-shared.pdf",
        "objstm-lin-firstpage-private-before-shared",
    );
}
```

- [ ] **Step 2: Add structural and strict Preserve tests**

Add immediately after the Generate tests:

```rust
// Preserve applies the same filtered-container classification but orders
// within each subsection by source ObjStm number.
#[test]
fn firstpage_private_container_precedes_shared_preserve_structurally() {
    let fixture = "objstm-lin-firstpage-private-before-shared-bearing.pdf";
    let stem = "objstm-lin-firstpage-private-before-shared-bearing";
    let actual = flpdf_linearized_objstm_preserve(fixture);
    let expected = golden_preserve(stem);
    report(
        fixture,
        &mask_id1(&actual),
        &mask_id1(&expected),
        "preserve structural",
    );
}

#[test]
fn firstpage_private_container_precedes_shared_preserve_byte_identical_to_qpdf() {
    let fixture = "objstm-lin-firstpage-private-before-shared-bearing.pdf";
    let stem = "objstm-lin-firstpage-private-before-shared-bearing";
    let actual = flpdf_linearized_objstm_preserve(fixture);
    let expected = golden_preserve(stem);
    report(fixture, &actual, &expected, "preserve strict");
}
```

- [ ] **Step 3: Run the tests and verify RED**

Run:

```bash
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_objstm_tests firstpage_private_container_precedes_shared \
  -- --nocapture
```

Expected: all four tests compile and fail with the parity reporter's first-byte
divergence. Confirm that the mismatch is layout/object numbering rather than
only `/ID[1]`; the structural tests must fail too.

Do not commit yet. Keep the RED tests in the worktree for Task 3.

---

### Task 3: Retain qpdf's Exact First-page Container Route

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs`
- Modify: `crates/flpdf/src/linearization/writer.rs`
- Test: `crates/flpdf/src/linearization/plan.rs`
- Test: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Interfaces:**
- Produces: `thumbnail_user_set(...) -> crate::Result<BTreeSet<ObjectRef>>`.
- Produces: `ContainerPart::{FirstPagePrivate, FirstPageShared, FirstPageOutlines}`.
- Preserves: `route_objstm_containers(...) -> crate::Result<Vec<ContainerPart>>`.

- [ ] **Step 1: Add focused route expectations before changing production code**

Update existing route assertions:

```rust
assert_eq!(
    routes,
    vec![
        ContainerPart::FirstPageShared,
        ContainerPart::OtherPagePrivate
    ]
);
```

and:

```rust
assert_eq!(
    routes,
    vec![
        ContainerPart::FirstPageShared,
        ContainerPart::OtherPageShared
    ]
);
```

Update the `/UseOutlines` expectation to
`ContainerPart::FirstPageOutlines`.

Add focused tests using existing synthetic fixtures:

```rust
#[test]
fn route_objstm_containers_distinguishes_first_page_private_and_shared() {
    let mut pdf =
        Pdf::open(Cursor::new(thumb_first_page_shared_pdf_bytes())).unwrap();
    let routes = route_objstm_containers(
        &mut pdf,
        &[
            vec![ObjectRef::new(7, 0)],
            vec![ObjectRef::new(5, 0)],
        ],
    )
    .unwrap();
    assert_eq!(
        routes,
        vec![
            ContainerPart::FirstPagePrivate,
            ContainerPart::FirstPageShared,
        ]
    );
}

#[test]
fn route_objstm_containers_keeps_same_page_self_thumb_private() {
    let mut pdf =
        Pdf::open(Cursor::new(self_thumb_first_page_private_pdf_bytes())).unwrap();
    let routes =
        route_objstm_containers(&mut pdf, &[vec![ObjectRef::new(5, 0)]])
            .unwrap();
    assert_eq!(routes, vec![ContainerPart::FirstPagePrivate]);
}
```

- [ ] **Step 2: Run the focused unit test and verify RED**

Run:

```bash
cargo test -p flpdf --lib route_objstm_containers -- --nocapture
```

Expected: compilation fails because the three refined variants do not exist.
This is the focused TDD failure for the production change.

- [ ] **Step 3: Extract the classic thumbnail-user calculation**

Add this private helper near `document_other_set`:

```rust
fn thumbnail_user_set<R: Read + Seek>(
    pdf: &mut Pdf<R>,
    page_refs: &[ObjectRef],
    first_page_set: &BTreeSet<ObjectRef>,
    other_page_closures: &[Vec<ObjectRef>],
    live: &BTreeSet<ObjectRef>,
) -> crate::Result<BTreeSet<ObjectRef>> {
    let page_tree = page_tree_node_refs(pdf)?;
    let mut thumb_refs = Vec::new();
    for (page_idx, &page_ref) in page_refs.iter().enumerate() {
        let thumb = pdf
            .resolve_borrowed(page_ref)?
            .as_dict()
            .and_then(|dict| dict.get("Thumb"))
            .and_then(|object| object.as_ref_id());
        thumb_refs.extend(thumb.map(|thumb_ref| (page_idx, thumb_ref)));
    }

    let mut result = BTreeSet::new();
    for (page_idx, thumb_ref) in thumb_refs {
        let closure =
            closure_from_seeds(pdf, vec![(thumb_ref, false)], &page_tree, live)?;
        for object_ref in closure {
            let visited_by_page = if page_idx == 0 {
                first_page_set.contains(&object_ref)
            } else {
                other_page_closures[page_idx - 1].contains(&object_ref)
            };
            if !visited_by_page {
                result.insert(object_ref);
            }
        }
    }
    Ok(result)
}
```

Replace the Step 4b inline block in `LinearizationPlan::from_pdf` with:

```rust
let thumb_shared_set = thumbnail_user_set(
    pdf,
    &page_refs,
    &first_page_set,
    &other_page_closures,
    &live,
)?;
```

Retain the existing qpdf explanation above the call, but move helper-specific
details into the helper's rustdoc. This is a mechanical extraction; run the
existing thumbnail unit and byte tests before changing routing:

```bash
cargo test -p flpdf --lib thumb_first_page
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_objstm_tests thumb_firstpage_shared
```

Expected: PASS, proving the extraction did not change classic behavior.

- [ ] **Step 4: Refine `ContainerPart`**

Replace `FirstPage` with:

```rust
/// qpdf part 6 — every container member user is compatible with
/// `lc_first_page_private`.
FirstPagePrivate,
/// qpdf part 6 — the container reaches the first page and also has a
/// document-other, non-first-page, or thumbnail user.
FirstPageShared,
/// qpdf part 6 — an outline container when `/PageMode /UseOutlines`.
FirstPageOutlines,
```

Update rustdoc references so no stale `ContainerPart::FirstPage` remains.

- [ ] **Step 5: Make the router compute all qpdf union signals once**

In `route_objstm_containers`, retain each non-first-page closure:

```rust
let first_page_closure = match page_refs.first() {
    Some(&first_page) => compute_closure(pdf, first_page, &live, &resurrectable)?,
    None => Vec::new(),
};
let first_page_set: BTreeSet<ObjectRef> =
    first_page_closure.iter().copied().collect();
let mut other_page_closures = Vec::new();
```

Inside the non-first-page loop, insert references into
`referenced_pages` and then push `closure` into `other_page_closures`.
After the loop:

```rust
let thumbnail_user_set = thumbnail_user_set(
    pdf,
    &page_refs,
    &first_page_set,
    &other_page_closures,
    &live,
)?;
```

Replace the first-page routing portion with:

```rust
if !outline_set.is_empty() && members.iter().any(|m| outline_set.contains(m)) {
    return if outlines_first_page {
        ContainerPart::FirstPageOutlines
    } else {
        ContainerPart::Rest
    };
}
if members.iter().any(|m| open_doc_set.contains(m)) {
    return ContainerPart::OpenDocument;
}
if members.iter().any(|m| first_page_set.contains(m)) {
    let has_other_page = members.iter().any(|member| {
        referenced_pages
            .get(member)
            .is_some_and(|pages| pages.iter().any(|&page| page != 0))
    });
    let has_document_other =
        members.iter().any(|m| document_other_set.contains(m));
    let has_thumbnail =
        members.iter().any(|m| thumbnail_user_set.contains(m));
    return if has_other_page || has_document_other || has_thumbnail {
        ContainerPart::FirstPageShared
    } else {
        ContainerPart::FirstPagePrivate
    };
}
```

Leave the existing Part 7/8/9 logic unchanged.

- [ ] **Step 6: Use stable first-page route buckets in both modes**

Replace `part3_regular` with separate `part3_private` and `part3_shared`
vectors. Keep `part3_outlines`.

Change `push_routed_objstm_batch` arguments and match arms to:

```rust
ContainerPart::FirstPagePrivate => part3_private.push(members),
ContainerPart::FirstPageShared => part3_shared.push(members),
ContainerPart::FirstPageOutlines => part3_outlines.push(members),
```

The function no longer needs `outline_set`; remove that argument and the
member scan. In both Generate and Preserve, build:

```rust
let mut part3_batches = part3_private;
part3_batches.extend(part3_shared);
part3_batches.extend(part3_outlines);
```

Do not sort any of these buckets. Generate iteration order is fresh container
number order; Preserve iteration order is source container number order.

- [ ] **Step 7: Update the writer's exhaustive first-half match**

In `second_half_container_anchors`, replace the old unreachable arm with:

```rust
ContainerPart::OpenDocument
| ContainerPart::FirstPagePrivate
| ContainerPart::FirstPageShared
| ContainerPart::FirstPageOutlines => {
    unreachable!("first-half route in second-half ObjStm batches")
}
```

- [ ] **Step 8: Run focused tests and verify GREEN**

Run:

```bash
cargo fmt
cargo test -p flpdf --lib route_objstm_containers -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_objstm_tests firstpage_private_container_precedes_shared \
  -- --nocapture
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_objstm_tests thumb_firstpage_shared \
  -- --nocapture
```

Expected: all focused unit tests and all six focused parity tests pass.

- [ ] **Step 9: Confirm no stale collapsed route remains**

Run:

```bash
rg -n 'ContainerPart::FirstPage\\b|part3_regular|outline_set.*push_routed' \
  crates/flpdf/src crates/flpdf/tests
```

Expected: no matches.

- [ ] **Step 10: Commit the behavior and regression tests**

```bash
git add crates/flpdf/src/linearization/plan.rs \
  crates/flpdf/src/linearization/writer.rs \
  crates/flpdf/tests/cmp_linearize_objstm_tests.rs
git commit -m "fix(linearize): order first-page ObjStm containers like qpdf"
```

---

### Task 4: Full Verification, Coverage, and Delivery

**Files:**
- Modify only if coverage reveals a genuinely untested route:
  `crates/flpdf/src/linearization/plan.rs`
- Modify only if a test is added:
  `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Interfaces:**
- Produces: clean CI-equivalent evidence, 100% changed-line coverage, closed Beads state, and a pushed branch.

- [ ] **Step 1: Run formatting and focused compatibility suites**

Run:

```bash
cargo fmt --all -- --check
cargo test -p flpdf --features qpdf-zlib-compat \
  --test cmp_linearize_objstm_tests
cargo test -p flpdf --test linearize_objstm_generate_tests
```

Expected: all commands exit zero.

- [ ] **Step 2: Run crate and workspace tests**

Run:

```bash
cargo test -p flpdf
cargo test -p flpdf-cli
cargo test
```

Expected: all tests pass.

- [ ] **Step 3: Run CI lint and documentation gates**

Run:

```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc --workspace --no-deps --document-private-items
```

Expected: both commands exit zero with no warnings.

- [ ] **Step 4: Validate both new outputs with qpdf**

Run:

```bash
qpdf --check-linearization \
  tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf
qpdf --check-linearization \
  tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf
qpdf --check \
  tests/golden/references/objstm-lin-firstpage-private-before-shared/linearize-objstm.pdf
qpdf --check \
  tests/golden/references/objstm-lin-firstpage-private-before-shared-bearing/linearize-objstm-preserve.pdf
```

Expected: all four checks exit zero.

- [ ] **Step 5: Run the authoritative committed-HEAD patch-coverage gate**

Run only with a clean worktree:

```bash
scripts/patch-coverage.sh --base main
```

Expected: `crates/flpdf/src` changed-line coverage is 100% and the script exits
zero. If an executable line is uncovered, add a focused test, commit it, and
rerun. Do not use `cov:ignore` for an ordinary reachable classification arm.

- [ ] **Step 6: Verify final diff and repository state**

Run:

```bash
git diff --check main...HEAD
git diff --stat main...HEAD
git status --short --branch
```

Expected: the diff contains only the approved design, implementation plan,
fixture/goldens, tests, and routing implementation; the worktree is clean.

- [ ] **Step 7: Close and push Beads state**

```bash
bd close flpdf-19ac --reason "qpdf 11.9.0 first-page private/shared/outline ObjStm ordering matched in Generate and Preserve; strict byte tests and 100% patch coverage pass"
bd dolt push
```

Expected: the issue is closed and the Dolt push succeeds.

- [ ] **Step 8: Push the git branch**

State before pushing that branch
`fix/flpdf-19ac-firstpage-objstm-order` contains the approved parity change,
then run:

```bash
git push -u origin fix/flpdf-19ac-firstpage-objstm-order
```

Expected: the remote push succeeds. Do not report completion before both the
Beads and git pushes have succeeded.
