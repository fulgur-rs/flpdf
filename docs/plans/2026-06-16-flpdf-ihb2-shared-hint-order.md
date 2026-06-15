# flpdf-ihb.2 — Shared-hint order vs physical object number Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `canonical_shared_hints` emit the first-page-section shared-object
hint entries in ascending **physical object-number** order (matching
`place_objstm_members_per_half`), so qpdf's positional walk attributes group
lengths to the right objects when a first-page shared object is ObjStm-ineligible.

**Architecture:** The fold loop is unchanged; after folding, the first-page
section (`shared_hints[0 .. part2.len()+part3.len()]` folded) is **stable-sorted
by physical object number**. Plain entries get their number from the post-placement
`RenumberMap` (threaded in as a new param); folded container entries already carry
their new number (`ObjectRef::new(container_num, u16::MAX)`). Part-8 (after-/E)
entries are left untouched. qpdf 11.9.0 is the oracle; verified byte-identical
under `qpdf-zlib-compat`.

**Tech Stack:** Rust, qpdf 11.9.0 (`--show-linearization`, `--check`),
`qpdf-zlib-compat` feature for byte comparison, `scripts/patch-coverage.sh`.

**Oracle ground truth (already captured):** 2-page fixture, page 0 shares a font
dict (eligible→container obj 11) and an image stream (ineligible→plain obj 10).
qpdf numbers `[page8, content9, image10, container11]` and lists the 4 first-page
shared slots in that physical order. flpdf currently emits `[…, container, image]`
→ `qpdf --check` reports "shared object 2/3 length mismatch". The fix is
oracle-confirmed.

---

### Task 1: Add reproduction fixture + qpdf golden + RED byte-identity test

**Files:**
- Modify: `tests/golden/regenerate.sh` (add `shared-stream-objstm.pdf` generator + golden step)
- Create: `tests/fixtures/compat/shared-stream-objstm.pdf` (committed)
- Create: `tests/golden/references/shared-stream-objstm/linearize-objstm.pdf` (committed)
- Test: `crates/flpdf/tests/cmp_linearize_objstm_tests.rs`

**Step 1: Add the fixture generator to `regenerate.sh`** (Phase 1, near `lone-flate-l9`):

```bash
if [[ ! -f "$FIX/shared-stream-objstm.pdf" ]]; then
    echo "Generating shared-stream-objstm.pdf ..."
    # 2-page PDF where page 0 shares with page 1 BOTH an ObjStm-eligible font
    # dict (obj 5) AND an ObjStm-ineligible image XObject stream (obj 6). The
    # eligible dict packs into a first-half container; the ineligible stream
    # stays a plain first-half object numbered BEFORE the container. Exercises
    # flpdf-ihb.2: folded shared-hint order must match physical object number.
    python3 - "$FIX/shared-stream-objstm.pdf" <<'PY'
import sys
def obj(n, body): return b"%d 0 obj\n" % n + body + b"\nendobj\n"
img = b"\xff"
c0 = b"BT /F1 12 Tf 100 700 Td (Page0) Tj ET\nq 1 0 0 1 0 0 cm /Im0 Do Q\n"
c1 = b"BT /F1 12 Tf 100 700 Td (Page1) Tj ET\nq 1 0 0 1 0 0 cm /Im0 Do Q\n"
res = b"<< /Font << /F1 5 0 R >> /XObject << /Im0 6 0 R >> >>"
o1 = obj(1, b"<< /Type /Catalog /Pages 2 0 R >>")
o2 = obj(2, b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>")
o3 = obj(3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources " + res + b" /Contents 7 0 R >>")
o4 = obj(4, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources " + res + b" /Contents 8 0 R >>")
o5 = obj(5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
o6 = obj(6, b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length %d >>\nstream\n" % len(img) + img + b"\nendstream")
o7 = obj(7, b"<< /Length %d >>\nstream\n" % len(c0) + c0 + b"\nendstream")
o8 = obj(8, b"<< /Length %d >>\nstream\n" % len(c1) + c1 + b"\nendstream")
body = b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n"; offs=[]
for o in (o1,o2,o3,o4,o5,o6,o7,o8): offs.append(len(body)); body+=o
xref=len(body); n=len(offs)+1
body += b"xref\n0 %d\n0000000000 65535 f \n" % n
for off in offs: body += b"%010d 00000 n \n" % off
body += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (n, xref)
open(sys.argv[1],"wb").write(body)
PY
else
    echo "Skipping shared-stream-objstm.pdf (already exists)"
fi
```

And in Phase 2 (golden generation), mirroring the two/three-page objstm goldens:

```bash
mkdir -p "$REF/shared-stream-objstm"
qpdf --linearize --object-streams=generate --deterministic-id --warning-exit-0 \
    "$FIX/shared-stream-objstm.pdf" "$REF/shared-stream-objstm/linearize-objstm.pdf"
echo "shared-stream-objstm/linearize-objstm.pdf"
```

**Step 2: Generate the fixture + golden**

Run: `bash tests/golden/regenerate.sh` (or the two added blocks directly).
Expected: `tests/fixtures/compat/shared-stream-objstm.pdf` and
`tests/golden/references/shared-stream-objstm/linearize-objstm.pdf` exist;
`qpdf --check` on the golden is warning-free (qpdf is correct).

**Step 3: Add structural + strict test cases** to `cmp_linearize_objstm_tests.rs`
(mirror `two_page_objstm_*`):

```rust
#[test]
fn shared_stream_objstm_structurally_byte_identical_to_qpdf() {
    assert_structural("shared-stream-objstm.pdf", "shared-stream-objstm");
}

#[test]
fn shared_stream_objstm_byte_identical_to_qpdf() {
    assert_strict("shared-stream-objstm.pdf", "shared-stream-objstm");
}
```

**Step 4: Run the new structural test — verify it FAILS (RED)**

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests shared_stream_objstm_structurally -- --nocapture`
Expected: FAIL — flpdf's shared hint table orders the container before the plain
image (the bug). This confirms the reproduction is wired to the gate.

**Step 5: Commit (RED test + fixtures)**

```bash
git add tests/golden/regenerate.sh tests/fixtures/compat/shared-stream-objstm.pdf \
        tests/golden/references/shared-stream-objstm/ \
        crates/flpdf/tests/cmp_linearize_objstm_tests.rs
git commit -m "test(flpdf): RED byte-identity gate for ineligible first-page shared stream order (flpdf-ihb.2)"
```

---

### Task 2: Thread RenumberMap into `canonical_shared_hints` and sort first-page section

**Files:**
- Modify: `crates/flpdf/src/linearization/plan.rs:830` (`canonical_shared_hints`)
- Modify: `crates/flpdf/src/linearization/hint_page.rs:307,330` (un-prefix `_renumber`, pass it)
- Modify: `crates/flpdf/src/linearization/hint_shared.rs:221` (pass `renumber`)
- Modify: `crates/flpdf/src/linearization/writer.rs:2393` (pass `renumber`)
- Modify: `crates/flpdf/src/linearization/plan.rs:3390,3424` (two unit tests)

**Step 1: Change the signature + body of `canonical_shared_hints`.** Add a
`renumber: &RenumberMap` param (import `RenumberMap` if needed). Track the
first-page-section boundary during the fold loop, then stable-sort that prefix by
physical object number:

```rust
pub(crate) fn canonical_shared_hints(
    &self,
    member_to_container: &BTreeMap<ObjectRef, (u32, u32)>,
    renumber: &RenumberMap,
) -> Vec<SharedObjectHintEntry> {
    if member_to_container.is_empty() {
        return self.shared_hints.clone();
    }

    // The first-page section of `shared_hints` is the leading
    // part2 ++ part3 entries; the trailing entries are Part-8
    // (`part4_other_pages_shared`, after /E).
    let first_page_input = self.part2_objects.len() + self.part3_objects.len();

    let mut container_pos: BTreeMap<u32, usize> = BTreeMap::new();
    let mut out: Vec<SharedObjectHintEntry> = Vec::with_capacity(self.shared_hints.len());
    let mut first_page_out_end: Option<usize> = None;

    for (input_idx, entry) in self.shared_hints.iter().enumerate() {
        if input_idx == first_page_input {
            // Crossed into the Part-8 region: freeze the first-page boundary.
            first_page_out_end = Some(out.len());
        }
        match member_to_container.get(&entry.object_ref) {
            Some(&(container_num, _idx)) => {
                if let Some(&pos) = container_pos.get(&container_num) {
                    let merged: &mut Vec<u32> = &mut out[pos].referencing_pages;
                    for &p in &entry.referencing_pages {
                        if let Err(insert_at) = merged.binary_search(&p) {
                            merged.insert(insert_at, p);
                        }
                    }
                } else {
                    let mut pages = entry.referencing_pages.clone();
                    pages.sort_unstable();
                    pages.dedup();
                    container_pos.insert(container_num, out.len());
                    out.push(SharedObjectHintEntry {
                        object_ref: ObjectRef::new(container_num, u16::MAX),
                        referencing_pages: pages,
                    });
                }
            }
            None => out.push(entry.clone()),
        }
    }

    // Reorder the first-page section to ascending physical object number, the
    // order qpdf's checkHSharedObject walks (positionally from the first page
    // object). `place_objstm_members_per_half` numbers the first half as
    // plain… then containers…, so a plain ObjStm-ineligible shared stream is
    // numbered BEFORE the container of the eligible dicts. A folded container
    // entry carries its new number with the sentinel generation u16::MAX; a
    // plain entry carries an original ref resolved through `renumber`.
    let boundary = first_page_out_end.unwrap_or(out.len());
    out[..boundary].sort_by_key(|e| {
        if e.object_ref.generation == u16::MAX {
            e.object_ref.number
        } else {
            renumber
                .new_for_original(e.object_ref)
                .map(|r| r.number)
                .unwrap_or(u32::MAX)
        }
    });

    out
}
```

Update the doc comment: state that first-page entries are emitted in physical
object-number order; cite qpdf's positional shared-object walk (no beads ID — this
is a `pub(crate)` item but keep the public-doc discipline). Keep the `member_to_container`
empty-map fast path returning the verbatim clone.

**Step 2: Update the three call sites** to pass the in-scope `RenumberMap`:
- `hint_page.rs`: rename param `_renumber` → `renumber`; call
  `plan.canonical_shared_hints(member_to_container, renumber)`.
- `hint_shared.rs:221`: `plan.canonical_shared_hints(member_to_container, renumber)`.
- `writer.rs:2393`: `plan.canonical_shared_hints(&objstm_layout.member_to_container, renumber)`.

**Step 3: Update the two unit tests in `plan.rs`:**
- `canonical_shared_hints_empty_map_is_identity` (3424): pass a renumber map. Since
  the empty-map branch returns before touching `renumber`, any map works:
  `let renumber = RenumberMap::from_plan(&plan); … canonical_shared_hints(&BTreeMap::new(), &renumber)`.
- `canonical_shared_hints_folds_members_and_unions_pages` (3390): build
  `let renumber = RenumberMap::from_plan(&plan);` from the same `plan`. The plan's
  part2 = `[page(3), content(9)]`, part3 = `[font_dict(1), font(2)]` (both in
  container 12). `from_plan` assigns ascending physical numbers so page<content<container;
  the sort therefore keeps `[page, content, container(12,MAX)]` — the existing
  assertions still hold. **Verify** by running; if `from_plan`'s numbering reorders
  page/content, adjust the asserted order to the physical order (not the input order)
  — physical order is the contract under test.

**Step 4: Run unit tests + the RED byte-identity test — verify GREEN**

Run: `cargo test -p flpdf --features qpdf-zlib-compat canonical_shared_hints`
Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests shared_stream_objstm`
Expected: all PASS (structural + strict).

**Step 5: Commit (the fix)**

```bash
git add crates/flpdf/src/linearization/
git commit -m "fix(flpdf): order first-page shared-hint entries by physical object number (flpdf-ihb.2)"
```

---

### Task 3: Full verification gates

**Step 1: No-regression byte identity** (the critical gate):

Run: `cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests --test cmp_linearize_tests`
Expected: `cmp_linearize_objstm_tests` 6 passed (was 4), `cmp_linearize_tests` 9 passed.

**Step 2: qpdf checker is warning-free on the new fixture**

```bash
target/debug/flpdf rewrite --linearize --object-streams=generate --deterministic-id \
    tests/fixtures/compat/shared-stream-objstm.pdf /tmp/ihb2-check.pdf   # build the bin first if needed
qpdf --check /tmp/ihb2-check.pdf
```
Expected: "File is linearized", NO "shared object … length mismatch" warnings.
(Build the binary in this worktree with `--features qpdf-zlib-compat` for a byte-exact
artifact, though the checker result is backend-independent.)

**Step 3: Full flpdf suite (default features)**

Run: `cargo test -p flpdf`
Expected: all green.

**Step 4: Patch coverage (WITHOUT qpdf-zlib-compat — miniz baseline)**

```bash
git add -A && git commit -m "wip" --no-verify   # coverage gates HEAD; keep tree clean
scripts/patch-coverage.sh --base main
```
Expected: 100% of changed `crates/flpdf/src/` lines covered. The new sort closure's
`u16::MAX` (container) and `else` (plain) arms are both exercised by the new fixture
(it has both a container and a plain ineligible shared stream); the `.unwrap_or(u32::MAX)`
arm is unreachable for live first-page objects — if it shows uncovered, add
`// cov:ignore: plain first-page shared objects are always present in the post-placement
RenumberMap; a missing entry is a planner/renumber inconsistency` and note it in the PR.

**Step 5: fmt + clippy + doc link gate**

```bash
cargo fmt --check
cargo clippy -p flpdf --all-targets
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" \
  cargo doc -p flpdf --no-deps --document-private-items
```
Expected: clean.

**Step 6: Final commit (squash the wip)**

```bash
git reset --soft HEAD~1   # drop the wip commit, keep changes staged if any remained
git status                # confirm clean / intended commits only
```
(No extra changes expected after Task 2's commit; the wip was only for the coverage gate.)
