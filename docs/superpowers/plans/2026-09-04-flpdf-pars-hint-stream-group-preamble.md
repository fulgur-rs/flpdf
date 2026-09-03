# qpdf-shaped shared-object hint encoding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove flpdf's latent shared-object groups pre-pass so the linearization hint stream has exactly qpdf 11.9.0's shared-object wire layout.

**Architecture:** Keep SharedObjectHintTable as the seven-field header plus per-shared-object entries. The existing bits_group_object_count header field remains qpdf's nbits_nobjects width and is used only by the nobjects_minus_one column. The encoder emits the three qpdf columns directly, preserving their existing byte-alignment and signature rules.

**Tech Stack:** Rust workspace, cargo test, cargo fmt, qpdf 11.9.0 source at /home/ubuntu/.cache/flpdf/qpdf-11.9.0, and the in-crate qpdf-shaped hint decoder in crates/flpdf/src/linearization/show.rs.

---

## File map

- Modify crates/flpdf/src/linearization/hint_shared.rs: remove the phantom group-entry data type and field; retain the qpdf header/entry data and update its unit tests and documentation.
- Modify crates/flpdf/src/linearization/hint_stream.rs: add the RED regression test, delete the phantom group encoder, and update encoder fixtures/comments.
- Modify crates/flpdf/src/linearization/show.rs: update qpdf-shaped test fixtures to construct only the header and shared-object entries; retain the existing decoder unchanged.
- Modify crates/flpdf/src/linearization/mod.rs: remove the obsolete SharedGroupEntry re-export.
- Verify docs/qpdf-correspondence.md: its linearization correspondence must not claim that flpdf serializes a separate group column; the current source search is expected to find no such claim, so no edit is expected.
- No changes to object classification, linearization part assignment, overflow-stream splitting, compression, or the public qpdf decoder contract.

## Task 1: Add and verify the failing qpdf-layout regression

**Files:**
- Modify: crates/flpdf/src/linearization/hint_stream.rs in the #[cfg(test)] mod tests module near nonzero_tables.

- [ ] **Step 1: Add a non-zero-width qpdf-column test before production edits.**

Add this test using the current SharedGroupEntry field. The distinct values make the current extra group byte observable through the existing qpdf-shaped reader:

~~~rust
#[test]
fn shared_section_emits_qpdf_columns_without_group_preamble() {
    let (page_offset, mut shared_object) = minimal_tables();
    shared_object.header.first_page_entries = 1;
    shared_object.header.section_entries = 1;
    shared_object.header.bits_group_object_count = 8;
    shared_object.header.bits_length_delta = 8;
    shared_object.groups = vec![SharedGroupEntry { object_count: 0xe1 }];
    shared_object.objects = vec![SharedObjectEntry {
        length_minus_least: 0x2a,
        signature_present: false,
        signature: None,
        nobjects_minus_one: 0x3c,
    }];

    let encoded = encode_hint_stream(&page_offset, &shared_object, None).expect("encode");
    let decoded = crate::linearization::show::read_h_shared_object(
        &encoded.uncompressed[encoded.shared_section_offset_in_uncompressed..],
    )
    .expect("decode qpdf-shaped shared section");
    let entry = &decoded.entries[0];

    assert_eq!(entry.delta_group_length, 0x2a);
    assert!(!entry.signature_present);
    assert_eq!(entry.nobjects_minus_one, 0x3c);
}
~~~

- [ ] **Step 2: Run the new test and confirm the expected RED failure.**

Run:

~~~bash
cargo test -p flpdf --lib shared_section_emits_qpdf_columns_without_group_preamble -- --nocapture
~~~

Expected: the test fails at delta_group_length because the current encoder emits 0xe1 from the phantom group pre-pass before the expected 0x2a value. Do not change production code until this failure is observed.

## Task 2: Remove the non-qpdf group representation

**Files:**
- Modify: crates/flpdf/src/linearization/hint_shared.rs
- Modify: crates/flpdf/src/linearization/hint_stream.rs
- Modify: crates/flpdf/src/linearization/show.rs
- Modify: crates/flpdf/src/linearization/mod.rs

- [ ] **Step 1: Delete the standalone group type and table field.**

In hint_shared.rs, delete SharedGroupEntry and change the table from:

~~~rust
pub struct SharedObjectHintTable {
    pub header: SharedObjectHeader,
    pub groups: Vec<SharedGroupEntry>,
    pub objects: Vec<SharedObjectEntry>,
}
~~~

to:

~~~rust
pub struct SharedObjectHintTable {
    pub header: SharedObjectHeader,
    pub objects: Vec<SharedObjectEntry>,
}
~~~

Remove groups: vec![] from the degenerate constructor and remove the let groups block from from_plan. The returned value must contain only header and objects.

- [ ] **Step 2: Update data-model documentation and unit tests.**

Rewrite the hint_shared.rs module/table comments so the serialized shape is a seven-field header followed by the three per-object columns. State that the one-object-per-group invariant is represented by SharedObjectEntry::nobjects_minus_one = 0, not by a stored group vector.

Rename or replace group-vector assertions with these assertions where the same invariant is being tested:

~~~rust
assert!(table.objects.is_empty(), "no shared objects -> entries must be empty");
assert_eq!(table.objects.len(), plan.shared_hints.len());
for entry in &table.objects {
    assert_eq!(entry.nobjects_minus_one, 0);
}
~~~

Delete tests whose only assertion is table.groups.len() or group.object_count; retain the corresponding header, object-count, and signature tests.

- [ ] **Step 3: Update all qpdf-shaped test literals and imports.**

Remove SharedGroupEntry from imports and remove groups fields in hint_stream.rs and show.rs. Keep bits_group_object_count and nobjects_minus_one in fixtures that exercise the qpdf column decoder. Replace comments that describe the old dormant pre-pass with the direct qpdf column order.

- [ ] **Step 4: Remove the obsolete public re-export.**

In crates/flpdf/src/linearization/mod.rs, replace the re-export with:

~~~rust
pub use hint_shared::{SharedObjectEntry, SharedObjectHeader, SharedObjectHintTable};
~~~

## Task 3: Make the encoder emit only qpdf's three columns

**Files:**
- Modify: crates/flpdf/src/linearization/hint_stream.rs

- [ ] **Step 1: Delete the phantom group encoder.**

Delete encode_shared_object_groups. encode_shared_object_entries remains the owner of delta_group_length, signature_present plus inline signatures, and nobjects_minus_one, and continues to flush at the same column boundaries.

- [ ] **Step 2: Remove its call from encode_shared_section.**

The function must become:

~~~rust
fn encode_shared_section(
    writer: &mut BitWriter<'_>,
    shared_object: &SharedObjectHintTable,
) -> crate::Result<()> {
    encode_shared_object_header(writer, shared_object)?;
    writer.flush()?;
    encode_shared_object_entries(writer, shared_object)?;
    writer.flush()?;
    Ok(())
}
~~~

- [ ] **Step 3: Run the RED test again and confirm GREEN.**

Run the same command from Task 1. Expected: the test passes and reports one passed test. This confirms that read_h_shared_object sees 0x2a, false, and 0x3c in qpdf order.

## Task 4: Verify the complete hint-table surface

**Files:**
- Test: crates/flpdf/src/linearization/hint_shared.rs
- Test: crates/flpdf/src/linearization/hint_stream.rs
- Test: crates/flpdf/src/linearization/show.rs

- [ ] **Step 1: Run focused linearization unit tests.**

~~~bash
cargo test -p flpdf --lib linearization::hint_shared
cargo test -p flpdf --lib linearization::hint_stream
cargo test -p flpdf --lib linearization::show
~~~

Expected: all selected tests pass with zero failures; existing qpdf-shaped round-trip tests continue to decode shared lengths, signatures, and nobjects_minus_one.

- [ ] **Step 2: Check for stale group representation references.**

~~~bash
rg -n 'SharedGroupEntry|\.groups|encode_shared_object_groups|groups pre-pass|groups column' crates/flpdf/src/linearization docs/qpdf-correspondence.md
~~~

Expected: no matches in production code or documentation. The generic word groups in unrelated ObjStm planning code is outside this issue and is not part of this check's target.

- [ ] **Step 3: Verify the qpdf source remains the documented oracle.**

~~~bash
sed -n '374,407p' /home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDF_linearization.cc
sed -n '1569,1606p' /home/ubuntu/.cache/flpdf/qpdf-11.9.0/libqpdf/QPDF_linearization.cc
~~~

Expected: qpdf reads and computes only the header plus per-entry columns; no change is made in the pinned qpdf mirror.

## Task 5: Run repository quality gates and inspect scope

- [ ] **Step 1: Run formatting and static qpdf checks.**

~~~bash
cargo fmt --all -- --check
python3 scripts/check-qpdf-deviation-markers.py --check
python3 scripts/qpdf-module-docs.py --check
~~~

Expected: all commands exit 0. No deviation marker is added because this change removes a qpdf-incompatible dormant representation.

- [ ] **Step 2: Run the relevant linearization and byte-parity tests.**

~~~bash
cargo test -p flpdf --features qpdf-zlib-compat --test cmp_linearize_objstm_tests
cargo test -p flpdf --features qpdf-zlib-compat --test linearize_objstm_generate_tests
cargo test -p flpdf --features qpdf-zlib-compat --test show_linearization_tests
cargo test -p flpdf --features qpdf-zlib-compat --test writer_linearization_route_tests
~~~

Expected: each existing test binary passes. These targets cover compressed-object linearization, generated object streams, hint-table display/decoding, and the writer linearization route.

- [ ] **Step 3: Run workspace lint, documentation, and tests.**

~~~bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags' cargo doc --workspace --no-deps --document-private-items
cargo test --workspace --all-features
~~~

Expected: all commands exit 0 with no lint or rustdoc errors.

- [ ] **Step 4: Inspect the final diff.**

~~~bash
git diff --check
git diff --stat
git status --short --branch
~~~

Expected: only the hint shared-table implementation/tests/docs plus the already-committed spec and plan are changed in this worktree. Do not stage unrelated generated files.

## Task 6: Record completion and publish the bounded change

- [ ] **Step 1: Commit the implementation.**

~~~bash
git add crates/flpdf/src/linearization/hint_shared.rs \
  crates/flpdf/src/linearization/hint_stream.rs \
  crates/flpdf/src/linearization/show.rs \
  crates/flpdf/src/linearization/mod.rs
git commit -m "fix(linearization): remove phantom shared hint groups"
~~~

- [ ] **Step 2: Append implementation evidence to Beads and read it back.**

Append a dated note to flpdf-pars containing the qpdf source lines, the RED/GREEN test name, focused/full verification commands, and the implementation commit. Then run:

~~~bash
bd show flpdf-pars --long
bd dep cycles
~~~

Expected: the issue is still associated with the exact implementation commit, no dependency cycle exists, and no unrelated issue is changed.

- [ ] **Step 3: Close the completed issue and verify closure.**

~~~bash
bd close flpdf-pars --reason "Implemented qpdf 11.9.0 shared-object hint layout: removed the non-qpdf groups pre-pass and retained only the delta length, signature, and nobjects columns; RED/GREEN and workspace verification passed in the implementation commit."
bd show flpdf-pars --short
~~~

Expected: flpdf-pars is CLOSED with the implementation reason preserved.

- [ ] **Step 4: Persist Beads and push the implementation branch.**

~~~bash
bd dolt push
git push -u origin fix/flpdf-pars-hint-groups
~~~

Expected: Beads prints Push complete. and git push succeeds. Finish with git status --short --branch; preserve any pre-existing main-checkout artifacts such as a.pdf.
