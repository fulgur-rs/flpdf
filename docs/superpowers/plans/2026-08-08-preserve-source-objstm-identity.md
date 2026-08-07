# Preserve Source ObjStm Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve a real source ObjStm as the identity of its reconstructed plain-writer container while keeping Generate containers explicitly synthetic.

**Architecture:** Introduce a source-aware group enum only for the qpdf-shaped plain-writer path, leaving the retained legacy `PackingPlan` unchanged. Generalize `GenerateRenumber` into `ObjectStreamRenumber`, activate a source-backed group from either its container or a member, and give source-container traversal a dedicated `/Extends`-only role. Carry the origin into `PlainWritePlan` so validation and later consumers can distinguish a reconstructed source container from a synthetic one.

**Tech Stack:** Rust 2024 workspace, `cargo test`, pinned qpdf 11.9.0 source and live behavior as semantic oracle, Beads issue `flpdf-um4z`.

## Global Constraints

- qpdf 11.9.0 responsibility boundaries and observable behavior are authoritative.
- Keep `PackingPlan { batches, removed_refs }` and the retained legacy writer behavior unchanged.
- Represent source-backed and synthetic containers as enum variants; do not add sentinels or parallel vectors.
- Follow only an indirect `/Extends` edge from a reconstructed source ObjStm; do not traverse other original container dictionary references.
- Keep structural body emission, `/Extends` serialization, xref allocation, linearized, QDF, encryption, and legacy writer migration out of scope.
- Use strict RED -> GREEN -> REFACTOR cycles. Every production change follows a test that failed for the intended missing behavior.

---

### Task 1: Source-aware Preserve group model

**Files:**
- Modify: `crates/flpdf/src/writer/object_streams.rs`

**Interfaces:**
- Consumes: existing `ObjectRef`, `CompressiblePlan`, `plan_preserve`, and `one_objstm_pdf_n` fixture helper.
- Produces: `ObjectStreamGroup`, `ObjectStreamPlan`, and the internal `plan_qpdf_preserve_groups(...) -> crate::Result<ObjectStreamPlan>` for Tasks 2 and 3. The existing batch-returning consumer entry point remains unchanged until the atomic plain-plan cutover in Task 3.

- [x] **Step 1: Write the failing Preserve identity/order test**

Add a unit test beside `planner_preserve_mode_reuses_source_membership`. The production break it catches is discarding source container `4 0 R` or retaining xref index order instead of qpdf's ascending `ObjectRef` order.

```rust
#[test]
fn qpdf_preserve_plan_retains_source_container_and_sorted_members() {
    let mut pdf = open_pdf(one_objstm_pdf_n(&[b"(hello)"]));

    let plan = plan_qpdf_preserve_groups(&mut pdf).unwrap();

    assert_eq!(
        plan.groups,
        vec![ObjectStreamGroup::SourceBacked {
            source: ObjectRef::new(4, 0),
            members: vec![ObjectRef::new(2, 0), ObjectRef::new(3, 0)],
        }]
    );
}
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf writer::object_streams::tests::qpdf_preserve_plan_retains_source_container_and_sorted_members -- --exact
```

Expected: compilation fails because `ObjectStreamPlan`, `ObjectStreamGroup`, `plan_qpdf_preserve_groups`, and `groups` do not exist. The existing batch-only plan must not satisfy the assertion.

- [x] **Step 3: Add the explicit group and plan types**

Add beside the legacy `PackingPlan`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectStreamGroup {
    SourceBacked {
        source: ObjectRef,
        members: Vec<ObjectRef>,
    },
    Synthetic {
        members: Vec<ObjectRef>,
    },
}

impl ObjectStreamGroup {
    pub(crate) fn members(&self) -> &[ObjectRef] {
        match self {
            Self::SourceBacked { members, .. } | Self::Synthetic { members } => members,
        }
    }

    pub(crate) fn members_mut(&mut self) -> &mut Vec<ObjectRef> {
        match self {
            Self::SourceBacked { members, .. } | Self::Synthetic { members } => members,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ObjectStreamPlan {
    pub(crate) groups: Vec<ObjectStreamGroup>,
    pub(crate) removed_refs: BTreeSet<ObjectRef>,
}
```

Keep `PackingPlan` and `plan_object_streams` unchanged.

- [x] **Step 4: Make only the qpdf Preserve planner source-aware**

Add `plan_qpdf_preserve_groups` returning `ObjectStreamPlan`. Build its groups directly from `pdf.source_xref_entries()`:

```rust
let mut by_container: BTreeMap<ObjectRef, Vec<ObjectRef>> = BTreeMap::new();
for (member, entry) in pdf.source_xref_entries() {
    if let XrefEntry::Compressed { stream, .. } = entry {
        by_container
            .entry(ObjectRef::new(stream, 0))
            .or_default()
            .push(member);
    }
}
```

For each container, retain only members in the existing compressible eligible set and passing the existing ObjStm eligibility and indirect-length exclusions, then sort by `(number, generation)`. Emit only non-empty `SourceBacked { source, members }` groups and preserve `compressible.removed_refs`.

Do not change or redirect the existing batch-returning `plan_qpdf_preserve_object_streams` in this task. It remains the production consumer until Task 3, which removes it while renaming `plan_qpdf_preserve_groups` to the final entry-point name.

- [x] **Step 5: Run the focused test and related planner regression**

Run:

```bash
cargo test -p flpdf writer::object_streams::tests::qpdf_preserve_plan_retains_source_container_and_sorted_members -- --exact
cargo test -p flpdf writer::object_streams::tests::planner_preserve_mode_reuses_source_membership -- --exact
```

Expected: both tests pass; the first proves the new qpdf-shaped plan and the second proves the legacy `PackingPlan` behavior stayed intact.

- [x] **Step 6: Commit Task 1**

```bash
git add crates/flpdf/src/writer/object_streams.rs
git commit -m "refactor(writer): retain Preserve ObjStm group identity"
```

---

### Task 2: Object-stream-aware renumbering and `/Extends` reachability

**Files:**
- Modify: `crates/flpdf/src/rewrite_renumber.rs`
- Modify: `crates/flpdf/src/writer/object_streams.rs` (Generate renumber regression call site only)
- Modify: `crates/flpdf/src/writer/plain/plan.rs` (mechanical type rename and Synthetic adaptation only)

**Interfaces:**
- Consumes: `ObjectStreamGroup` from Task 1 and existing qpdf-shaped trailer/reference collection.
- Produces: `ObjectStreamRenumber::build(pdf, groups, skip_length, removed_refs)`, `container_number`, `container_numbers`, `pairs`, and `NewNumberLookup` for Task 3.

- [x] **Step 1: Write failing container-first/member-first tests**

Use existing `build_raw_pdf` in `rewrite_renumber.rs`. The production break caught is treating source-backed groups as synthetic or allocating a second placement when the source container is encountered first.

```rust
fn source_group(source: u32, members: &[u32]) -> ObjectStreamGroup {
    ObjectStreamGroup::SourceBacked {
        source: ObjectRef::new(source, 0),
        members: members.iter().map(|&n| ObjectRef::new(n, 0)).collect(),
    }
}

#[test]
fn source_backed_member_first_and_container_first_number_identically() {
    let bodies = |first: u32| {
        let catalog = format!("<< /Type /Catalog /First {first} 0 R >>").into_bytes();
        build_raw_pdf(&[
            (1, catalog.as_slice()),
            (2, b"<< /Value 2 >>"),
            (3, b"<< /Value 3 >>"),
            (4, b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream"),
        ])
    };
    let groups = vec![source_group(4, &[3, 2])];

    for first in [2, 4] {
        let bytes = bodies(first);
        let mut pdf = Pdf::open_mem_owned(bytes).unwrap();
        let map = ObjectStreamRenumber::build(&mut pdf, &groups, true, &BTreeSet::new()).unwrap();
        assert_eq!(map.new_for_original(ObjectRef::new(4, 0)), Some(ObjectRef::new(2, 0)));
        assert_eq!(map.new_for_original(ObjectRef::new(2, 0)), Some(ObjectRef::new(3, 0)));
        assert_eq!(map.new_for_original(ObjectRef::new(3, 0)), Some(ObjectRef::new(4, 0)));
        assert_eq!(map.container_numbers(), vec![2]);
    }
}
```

Use owned `Vec<u8>` bodies if the existing helper's borrowed-slice signature requires values to outlive the call.

- [x] **Step 2: Run the numbering test and verify RED**

Run:

```bash
cargo test -p flpdf rewrite_renumber::tests::source_backed_member_first_and_container_first_number_identically -- --exact
```

Expected: compilation fails because `ObjectStreamRenumber` is absent and `GenerateRenumber` cannot accept `ObjectStreamGroup`.

- [x] **Step 3: Generalize the renumber type and validate groups**

Rename `GenerateRenumber` to `ObjectStreamRenumber` without a compatibility alias. Update `NewNumberLookup` and module documentation. Change `build` to accept `&[ObjectStreamGroup]` and precompute:

```rust
let mut member_to_group = HashMap::<ObjectRef, usize>::new();
let mut source_to_group = HashMap::<ObjectRef, usize>::new();
let mut groups_sorted = Vec::<Vec<ObjectRef>>::with_capacity(groups.len());
```

For every group, clone and ascending-sort `members()`. Return `Error::Unsupported` if a member occurs in two groups, a source occurs in two source-backed groups, a source is also any member, or a group is empty. Do not overwrite an earlier hash-map entry.

Update the two existing production call sites in `writer/plain/plan.rs` only far enough to preserve current behavior and compile after the rename: convert each existing Preserve or Generate `Vec<Vec<ObjectRef>>` batch to `ObjectStreamGroup::Synthetic { members }` for `ObjectStreamRenumber::build`, while continuing to pass the original batches to `build_container_aware`. Change that helper's parameter type from `GenerateRenumber` to `ObjectStreamRenumber`. The source-aware Preserve cutover and planned origin remain Task 3 behavior, protected by its RED tests.

- [x] **Step 4: Implement idempotent source/synthetic activation**

Replace `enqueue_gen` with an object-stream enqueue helper. A group activation:

1. assigns the next number to the container and records `container_new[group_index]`;
2. inserts `source -> container_output` only for `SourceBacked`;
3. assigns consecutive numbers to sorted members;
4. queues every member as ordinary work;
5. queues the source container as special work only for `SourceBacked`.

Use an explicit queue role:

```rust
enum RenumberWork {
    Ordinary(ObjectRef),
    SourceContainer(ObjectRef),
}
```

The source/member lookup selects the same group, and `container_new[group_index].is_some()` makes activation idempotent. Synthetic groups never add a source mapping or source-container queue item.

- [x] **Step 5: Run the numbering test and verify GREEN**

Run the Step 2 command. Expected: pass with literal mappings `1->1`, source container `4->2`, member `2->3`, member `3->4` for both encounter orders.

- [x] **Step 6: Write failing `/Extends`-only reachability tests**

Add two real-PDF tests. The production breaks caught are walking `/Aux` from a reconstructed source container and failing to activate an `/Extends` target group.

```rust
#[test]
fn source_container_follows_only_indirect_extends() {
    let bytes = build_raw_pdf(&[
        (1, b"<< /Type /Catalog /First 2 0 R >>"),
        (2, b"<< /Value 2 >>"),
        (4, b"<< /Type /ObjStm /N 0 /First 0 /Length 0 /Extends 5 0 R /Aux 6 0 R >>\nstream\n\nendstream"),
        (5, b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream"),
        (6, b"<< /WronglyReachable true >>"),
        (7, b"<< /Value 7 >>"),
    ]);
    let groups = vec![source_group(4, &[2]), source_group(5, &[7])];
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let map = ObjectStreamRenumber::build(&mut pdf, &groups, true, &BTreeSet::new()).unwrap();

    assert_eq!(map.new_for_original(ObjectRef::new(5, 0)), Some(ObjectRef::new(4, 0)));
    assert_eq!(map.new_for_original(ObjectRef::new(7, 0)), Some(ObjectRef::new(5, 0)));
    assert_eq!(map.new_for_original(ObjectRef::new(6, 0)), None);
    assert_eq!(map.container_numbers(), vec![2, 4]);
}

#[test]
fn extends_target_without_retained_group_is_an_ordinary_source() {
    let bytes = build_raw_pdf(&[
        (1, b"<< /Type /Catalog /First 2 0 R >>"),
        (2, b"<< /Value 2 >>"),
        (4, b"<< /Type /ObjStm /N 0 /First 0 /Length 0 /Extends 5 0 R /Aux 6 0 R >>\nstream\n\nendstream"),
        (5, b"<< /Type /ObjStm /N 0 /First 0 /Length 0 >>\nstream\n\nendstream"),
        (6, b"<< /WronglyReachable true >>"),
    ]);
    let groups = vec![source_group(4, &[2])];
    let mut pdf = Pdf::open_mem_owned(bytes).unwrap();

    let map = ObjectStreamRenumber::build(&mut pdf, &groups, true, &BTreeSet::new()).unwrap();

    assert_eq!(map.new_for_original(ObjectRef::new(5, 0)), Some(ObjectRef::new(4, 0)));
    assert_eq!(map.new_for_original(ObjectRef::new(6, 0)), None);
    assert_eq!(map.container_numbers(), vec![2]);
}
```

- [x] **Step 7: Run the reachability tests and verify RED**

Run:

```bash
cargo test -p flpdf rewrite_renumber::tests::source_container_follows_only_indirect_extends -- --exact
cargo test -p flpdf rewrite_renumber::tests::extends_target_without_retained_group_is_an_ordinary_source -- --exact
```

Expected before the special role is implemented: `/Aux` receives a mapping or the `/Extends` target is absent/misclassified.

- [x] **Step 8: Implement the source-container work role**

For `RenumberWork::Ordinary`, retain the existing `pdf.resolve` plus `collect_qpdf_enqueue_refs` walk. For `RenumberWork::SourceContainer(source)`, resolve the source, require a stream body, inspect only `stream.dict.get("Extends")`, and enqueue it only when it is `Object::Reference`. Apply the same `removed_refs` filter before enqueue. Do not call `collect_qpdf_enqueue_refs` on the original container.

Queue members before the source-container role so member child references are numbered before `/Extends`, matching qpdf `writeObjectStream`.

- [x] **Step 9: Run all Task 2 tests and Generate regression**

Run:

```bash
cargo test -p flpdf rewrite_renumber::tests::source_backed_member_first_and_container_first_number_identically -- --exact
cargo test -p flpdf rewrite_renumber::tests::source_container_follows_only_indirect_extends -- --exact
cargo test -p flpdf rewrite_renumber::tests::extends_target_without_retained_group_is_an_ordinary_source -- --exact
cargo test -p flpdf writer::object_streams::tests::generate_renumber_matches_qpdf_on_130_page_reverse -- --exact
```

Expected: all pass. Update the Generate regression to wrap even-split batches as `ObjectStreamGroup::Synthetic { members }`; its measured mappings and container numbers must remain unchanged.

- [x] **Step 10: Commit Task 2**

```bash
git add crates/flpdf/src/rewrite_renumber.rs crates/flpdf/src/writer/object_streams.rs crates/flpdf/src/writer/plain/plan.rs
git commit -m "refactor(writer): renumber source-backed ObjStm groups"
```

---

### Task 3: Plain placement origin and validation

**Files:**
- Modify: `crates/flpdf/src/writer/object_streams.rs` (rename the source-aware helper to the final consumer entry point and remove the old batch-only specialized entry point)
- Modify: `crates/flpdf/src/writer/plain/plan.rs`
- Modify: `crates/flpdf/src/writer/plain/body.rs` (pattern compatibility only; no emission behavior change)

**Interfaces:**
- Consumes: `ObjectStreamPlan`, `ObjectStreamGroup`, and `ObjectStreamRenumber` from Tasks 1-2.
- Produces: `PlannedObjectStreamOrigin` retained in each `PlannedIndirectObject::ObjectStream` and validated source-container placement.

- [x] **Step 1: Write failing plain-plan source-origin test**

Extend `preserve_source_objstm_members_keep_one_container_and_indices`. The production break caught is a synthetic placement with no source identity or a duplicate ordinary `Source` placement for source container `1 0 R`.

```rust
let (origin, output, members) = plan.objects.iter().find_map(|object| match object {
    PlannedIndirectObject::ObjectStream { origin, output, members } => {
        Some((origin, *output, members))
    }
    _ => None,
}).expect("Preserve container");

assert_eq!(
    origin,
    &PlannedObjectStreamOrigin::SourceBacked(ObjectRef::new(1, 0))
);
assert_eq!(plan.old_to_new.get(&ObjectRef::new(1, 0)), Some(&output));
assert!(plan.objects.iter().all(|object| !matches!(
    object,
    PlannedIndirectObject::Source { source, .. } if *source == ObjectRef::new(1, 0)
)));
assert!(!members.is_empty());
```

- [x] **Step 2: Run the focused test and verify RED**

Run:

```bash
cargo test -p flpdf writer::plain::plan::tests::preserve_source_objstm_members_keep_one_container_and_indices -- --exact
```

Expected: compilation fails because `PlannedObjectStreamOrigin` and the `origin` field do not exist.

- [x] **Step 3: Carry explicit origin through the plain placement**

Add:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlannedObjectStreamOrigin {
    SourceBacked(ObjectRef),
    Synthetic,
}
```

Add `origin: PlannedObjectStreamOrigin` to `PlannedIndirectObject::ObjectStream`. Rename `plan_qpdf_preserve_groups` to `plan_qpdf_preserve_object_streams` and delete the old batch-returning specialized function. Generalize `build_container_aware` to accept `ObjectStreamRenumber` and `Vec<ObjectStreamGroup>`. Build `member_sources` from `group.members()` and a separate set of source-backed container sources; exclude both sets from ordinary `Source` placements. Convert each group variant to its matching planned origin while retaining mapped members sorted by output number.

Update every existing hand-constructed `PlannedIndirectObject::ObjectStream` in `plan.rs` tests with `origin: PlannedObjectStreamOrigin::Synthetic`; those tests exercise output/member validation independent of source identity. Use `SourceBacked` only in the new duplicate-placement test and in Preserve plans built from real source xref membership.

In Preserve mode, filter removed members through `group.members_mut()` and drop empty groups. Keep the existing classic fallback when all groups are empty and the source had no compressed objects. In Generate mode, wrap every even-split batch in `ObjectStreamGroup::Synthetic` before renumbering.

- [x] **Step 4: Update body pattern matches without changing serialization**

Change exact destructuring sites to ignore the new field:

```rust
PlannedIndirectObject::ObjectStream {
    output,
    members,
    ..
} => { /* existing body unchanged */ }
```

Do not read the origin in `body.rs`; `/Extends` serialization belongs to the dependent consumer issue.

- [x] **Step 5: Run the source-origin test and verify GREEN**

Run the Step 2 command. Expected: pass with source `1 0 R` mapped to the one reconstructed container and absent from ordinary `Source` placements.

- [x] **Step 6: Write failing duplicate-placement validation test**

The production break caught is accepting one source as both an ordinary object and the origin of a reconstructed ObjStm.

```rust
#[test]
fn validation_rejects_source_and_source_backed_container_for_same_source() {
    let container_source = ObjectRef::new(2, 0);
    let mut plan = plan_for_test(vec![
        source(1, 1),
        source(2, 3),
        PlannedIndirectObject::ObjectStream {
            origin: PlannedObjectStreamOrigin::SourceBacked(container_source),
            output: ObjectRef::new(2, 0),
            members: Vec::new(),
        },
    ]);
    plan.old_to_new.insert(container_source, ObjectRef::new(3, 0));
    plan.trailer.form = XrefForm::Stream;

    let error = plan.validate().unwrap_err();

    assert!(matches!(error, crate::Error::Unsupported(message)
        if message.contains("source 2 0 R has multiple placements")));
}
```

- [x] **Step 7: Run the validation test and verify RED**

Run:

```bash
cargo test -p flpdf writer::plain::plan::tests::validation_rejects_source_and_source_backed_container_for_same_source -- --exact
```

Expected: test fails because validation does not count the ObjectStream origin as a source placement.

- [x] **Step 8: Validate the source-backed origin**

In the ObjectStream arm of `PlainWritePlan::validate`, before member validation:

```rust
if let PlannedObjectStreamOrigin::SourceBacked(source) = origin {
    require_not_removed(&self.removed_refs, *source, "ObjStm source container")?;
    require_unique_source(&mut sources, *source)?;
    require_matching_mapping(&self.old_to_new, *source, *output)?;
}
```

Leave `Synthetic` out of source uniqueness and old-to-new completeness.

- [x] **Step 9: Run focused plain-plan tests and existing Preserve/Generate regressions**

Run:

```bash
cargo test -p flpdf writer::plain::plan::tests::validation_rejects_source_and_source_backed_container_for_same_source -- --exact
cargo test -p flpdf writer::plain::plan::tests::preserve_source_objstm_members_keep_one_container_and_indices -- --exact
cargo test -p flpdf writer::plain::plan::tests::preserve_without_source_objstm_uses_catalog_first_sources -- --exact
cargo test -p flpdf writer::plain::plan::tests::generate_plan_even_splits_132_eligible_objects -- --exact
```

Expected: all pass and `PlainWritePlan::validate()` accepts all valid plans.

- [x] **Step 10: Commit Task 3**

```bash
git add crates/flpdf/src/writer/object_streams.rs crates/flpdf/src/writer/plain/plan.rs crates/flpdf/src/writer/plain/body.rs
git commit -m "refactor(writer): place Preserve ObjStm by source identity"
```

---

### Task 4: Full verification and Beads evidence

**Files:**
- Modify: `docs/superpowers/plans/2026-08-08-preserve-source-objstm-identity.md` (check completed steps)
- Beads mutation: append verification evidence to `flpdf-um4z`; close only after every acceptance criterion is demonstrated.

**Interfaces:**
- Consumes: all production and test changes from Tasks 1-3.
- Produces: formatted, warning-clean, workspace-tested branch and persisted Beads evidence.

- [x] **Step 1: Run formatting and focused component tests**

```bash
cargo fmt -- --check
cargo test -p flpdf writer::object_streams::tests
cargo test -p flpdf rewrite_renumber::tests
cargo test -p flpdf writer::plain::plan::tests
```

Expected: all commands exit 0 with no failed tests.

- [x] **Step 2: Run crate and workspace verification**

```bash
cargo test -p flpdf
cargo test
```

Expected: both commands exit 0 with zero failures.

- [x] **Step 3: Review diff against every acceptance criterion**

Check `git diff origin/main...HEAD` and confirm literal evidence for:

- source-backed Preserve group identity and ascending member order;
- explicit source-backed/synthetic variants;
- one mapping and one placement for a source container;
- member-first/container-first identical numbering;
- `/Extends`-only source-container reachability;
- source-backed or ordinary placement of `/Extends` targets;
- ordinary placement when no retained group exists;
- planned origin retained for the later consumer;
- duplicate placement validation without panic;
- unchanged legacy Preserve and Generate regressions.

- [x] **Step 4: Record and persist Beads evidence**

Append the exact tests and oracle correspondence to `flpdf-um4z`, then close it only if all criteria above are met:

```bash
bd update flpdf-um4z --append-notes "Implementation 2026-08-08: added explicit SourceBacked/Synthetic groups, source-aware ObjectStreamRenumber with container/member encounter parity and indirect-/Extends-only reachability, and planned source-container validation. Verified cargo fmt -- --check, focused object_stream/renumber/plain-plan tests, cargo test -p flpdf, and cargo test."
bd close flpdf-um4z
bd dolt push
```

Read the issue back with `bd show flpdf-um4z --json` and verify `closed` plus the evidence text.

- [x] **Step 5: Commit plan completion and push the branch**

```bash
git add docs/superpowers/plans/2026-08-08-preserve-source-objstm-identity.md
git commit -m "docs: record source-aware Preserve verification"
git push
```

Read back the remote branch SHA and require it to equal local `HEAD` before handoff.

## Completion evidence

- Independent review: no Critical, Important, or Minor findings; Ready to merge.
- Focused tests: object-stream planner 52/52, renumber 38/38, plain plan 36/36.
- Oracle parity: Preserve 12/12 and Generate 9/9 byte-parity matrices.
- Quality gates: `cargo fmt -- --check`, `cargo test -p flpdf`, and `cargo test` passed.
- Patch coverage: 452 changed executable lines, 0 uncovered (100%) against `origin/main`.
- Beads: `flpdf-um4z` closed and pushed to the Dolt remote on 2026-08-08.
