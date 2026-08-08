# Effective Xref Free-Entry Cutover Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

Goal: Make LoadedXref.entries match qpdf 11.9.0 by retaining only live xref entries and keeping free object numbers in a construction-scoped deleted set.

Architecture: Add one registration state inside crates/flpdf/src/xref.rs that implements qpdf's exact-ObjectRef first-wins and object-number deletion rules. Classic-table free rows are deferred until after /XRefStm; xref-stream type 0 uses generation zero and ignores its generation field. Finalization emits the /Size warning, moves only live entries into LoadedXref, and clears the deleted set. Reader/cache consumers then rely on the effective table, while writer-side XrefEntry::Free assembly remains unchanged.

Tech Stack: Rust workspace, cargo test, cargo fmt, qpdf 11.9.0 pinned source and executable oracle, Beads.

---

### Task 1: Add RED coverage for qpdf effective-table semantics

Files:

- Modify crates/flpdf/tests/xref_tests.rs:295-342 for latest-free suppression and hybrid ordering.
- Modify crates/flpdf/tests/xref_tests.rs:1866-1895 for classic free-field behavior.
- Add tests in crates/flpdf/tests/xref_tests.rs for generations, wide type-0 generation, and /Size diagnostics.

- [ ] Step 1: Change the latest-free expectation to the qpdf effective-table contract.

Change loads_latest_xref_stream_free_entries_over_previous_live_entries so object 2 0 is absent rather than represented by XrefEntry::Free:

    assert_eq!(loaded.entries.get(&ObjectRef::new(2, 0)), None);

Keep the object 1 0 and xref-stream object assertions unchanged.

- [ ] Step 2: Make the existing hybrid fixture contain a classic free row.

In classic_xref_with_hybrid_only_entry, change the classic subsection from 0 2 to 0 3, add object 2 0 as a classic free row, and retain object 2 0 as a live row in the hybrid stream. Set the classic trailer /Size to 4. The relevant table bytes become:

    bytes.extend_from_slice(b"xref\n0 3\n0000000000 65535 f \n");
    bytes.extend_from_slice(format!("{catalog_offset:010} 00000 n \n").as_bytes());
    bytes.extend_from_slice(b"0000000000 00000 f \n");

The existing classic_xref_table_reads_entries_from_its_xrefstm assertion for object 2 0 is the RED assertion: the hybrid live row must survive the deferred classic free row.

- [ ] Step 3: Add a RED test for distinct generations across /Prev.

Add previous_xref_sections_retain_distinct_generations using two xref-stream sections. The newest stream must register object 1 2; its /Prev stream must register object 1 0. Assert both exact keys are present:

    assert!(matches!(
        loaded.entries.get(&ObjectRef::new(1, 2)),
        Some(XrefEntry::Uncompressed { .. })
    ));
    assert!(matches!(
        loaded.entries.get(&ObjectRef::new(1, 0)),
        Some(XrefEntry::Uncompressed { .. })
    ));

Use a local xref-stream builder with /W [1 4 2], encoding generation in the third field of type-1 rows. The old object-number merge rejects the older generation, so this test must fail before the implementation change.

- [ ] Step 4: Add a RED test for type-0 generation being ignored.

Add xref_stream_type_zero_ignores_a_wide_generation with /W [1 4 4] and a type-0 row whose third field is 0xffff_ffff. Assert the input loads and object 1 0 is absent:

    let loaded = load_xref_and_trailer(&mut Cursor::new(bytes))
        .expect("type-0 generation is ignored");
    assert_eq!(loaded.entries.get(&ObjectRef::new(1, 0)), None);

The old u16::try_from(field2) path must fail this test.

- [ ] Step 5: Add a RED test for /Size using a deleted object.

Add warns_when_xref_size_is_not_one_plus_the_highest_deleted_object with a classic table containing free object number 5 and trailer /Size 5. Assert that the effective table has no free entry and diagnostics contain the exact qpdf warning:

    assert!(loaded.entries.keys().all(|object_ref| object_ref.number != 5));
    assert!(loaded.repair_diagnostics.entries().iter().any(|diagnostic| {
        diagnostic.message
            == "reported number of objects (5) is not one plus the highest object number (5)"
            && diagnostic.offset.is_none()
    }));

- [ ] Step 6: Replace the stale classic-free overflow expectation.

Rename rejects_xref_table_free_next_overflow to classic_free_next_field_is_not_retained_or_u32_limited. Keep the 9999999999 field, but assert strict loading succeeds and object 1 0 is absent. qpdf parses the classic free field as an offset-sized integer and does not retain it in the effective table.

- [ ] Step 7: Run the focused tests and record RED failures.

    cargo test -p flpdf --test xref_tests loads_latest_xref_stream_free_entries_over_previous_live_entries
    cargo test -p flpdf --test xref_tests classic_xref_table_reads_entries_from_its_xrefstm
    cargo test -p flpdf --test xref_tests previous_xref_sections_retain_distinct_generations
    cargo test -p flpdf --test xref_tests xref_stream_type_zero_ignores_a_wide_generation
    cargo test -p flpdf --test xref_tests warns_when_xref_size_is_not_one_plus_the_highest_deleted_object
    cargo test -p flpdf --test xref_tests classic_free_next_field_is_not_retained_or_u32_limited

Expected result: the changed/new tests fail because the current reader stores XrefEntry::Free, rejects the wide generation, merges by object number, and does not emit the /Size warning.

- [ ] Step 8: Commit the RED tests.

    git add crates/flpdf/tests/xref_tests.rs
    git commit -m "test: pin effective xref free-entry semantics"

### Task 2: Implement qpdf registration and finalization in xref.rs

Files:

- Modify crates/flpdf/src/xref.rs:20-360 for shared construction state and section ordering.
- Modify crates/flpdf/src/xref.rs:762-1063 for classic and stream record parsing.
- Modify crates/flpdf/src/xref.rs:1260-end for private registration unit tests.

- [ ] Step 1: Add the private parsed-record and registration types.

Add these implementation types near XrefForm:

    enum ParsedXrefEntry {
        Live { object_ref: ObjectRef, entry: XrefEntry },
        Free { object_ref: ObjectRef },
    }

    #[derive(Debug, Default)]
    struct XrefRegistration {
        entries: BTreeMap<ObjectRef, XrefEntry>,
        deleted_objects: BTreeSet<u32>,
    }

    impl XrefRegistration {
        fn insert_xref_entry(&mut self, object_ref: ObjectRef, entry: XrefEntry) {
            if self.deleted_objects.contains(&object_ref.number) {
                return;
            }
            self.entries.entry(object_ref).or_insert(entry);
        }

        fn insert_free_xref_entry(&mut self, object_ref: ObjectRef) {
            if !self.entries.contains_key(&object_ref) {
                self.deleted_objects.insert(object_ref.number);
            }
        }

        fn snapshot(&self) -> BTreeMap<ObjectRef, XrefEntry> {
            self.entries.clone()
        }
    }

- [ ] Step 2: Convert classic-table parsing to live records plus deferred free refs.

Change parse_xref_table to return Vec<ParsedXrefEntry> and a trailer. For n, append a Live record. For f, read the ten-digit field as u64 only to consume and validate the fixed-width integer, discard it, and append Free with ObjectRef::new(first + index, generation). Do not construct XrefEntry::Free.

- [ ] Step 3: Convert xref-stream parsing to records with type-0 generation zero.

Change parse_xref_entries to return Vec<ParsedXrefEntry>. Continue reading all three fields so cursor advancement and width validation are unchanged. Use this mapping:

    0 => entries.push(ParsedXrefEntry::Free {
        object_ref: ObjectRef::new(object_number, 0),
    }),
    1 => entries.push(ParsedXrefEntry::Live {
        object_ref: ObjectRef::new(object_number, generation),
        entry: XrefEntry::Uncompressed { offset: field1 },
    }),
    2 => entries.push(ParsedXrefEntry::Live {
        object_ref: ObjectRef::new(object_number, 0),
        entry: XrefEntry::Compressed { stream, index },
    }),

The type-0 arm intentionally does not convert either free-list field to u32/u16; qpdf ignores both for this effective-table operation. Preserve the existing u16/u32 validation for type 1 and type 2 fields.

- [ ] Step 4: Thread one registration state through the latest section, hybrid stream, and /Prev.

Create let mut registration = XrefRegistration::default() in load_xref_state_with_options and pass &mut registration through parse_xref_from_start, merge_xref_stream_from_classic_trailer, and merge_previous_xref_sections.

For a classic section, register every live record immediately, parse the hybrid stream, then register the section's deferred free records. This keeps classic live rows before hybrid live rows and hybrid live rows before classic free rows. For a stream section, register records in stream order. For each older /Prev section, call the same parser with the same registration state; the registration methods supply qpdf's exact-key first-wins behavior.

Remove the existing object-number any(|entry_ref| entry_ref.number == ...) merge loops. After /Prev processing, assign loaded.loaded.entries = registration.snapshot().

- [ ] Step 5: Add final /Size warning and clear the construction-only set.

Add a helper that computes the maximum object number across registration.entries.keys() and registration.deleted_objects. If the latest trailer has an integer /Size and either /Size < 1 or /Size - 1 != max_obj, append:

    Diagnostic::warning(
        format!(
            "reported number of objects ({size}) is not one plus the highest object number ({max_obj})"
        ),
        None,
    )

Call it after the complete /Prev chain and before clearing registration.deleted_objects. The returned LoadedXref must contain only registration.entries; no deleted-set state may escape the loader.

- [ ] Step 6: Add private registration unit tests.

In the existing xref.rs test module, test that a live entry blocks a later free record for the exact generation, a free record blocks a later live entry for the same object number, and a live entry for a different generation is retained when the object number is already present:

    let object_zero = ObjectRef::new(7, 0);
    let object_two = ObjectRef::new(7, 2);
    registration.insert_xref_entry(object_zero, XrefEntry::Uncompressed { offset: 10 });
    registration.insert_free_xref_entry(object_zero);
    registration.insert_xref_entry(object_two, XrefEntry::Uncompressed { offset: 20 });
    assert!(registration.entries.contains_key(&object_zero));
    assert!(registration.entries.contains_key(&object_two));
    assert!(registration.deleted_objects.is_empty());

Add the inverse free-first assertion for object 8 0, verifying that a later live entry is suppressed and the deleted set contains only object number 8.

- [ ] Step 7: Run xref tests GREEN and commit the loader.

    cargo fmt --all
    cargo test -p flpdf --test xref_tests
    cargo test -p flpdf xref::tests

Expected result: all xref tests pass, including every RED test from Task 1.

    git add crates/flpdf/src/xref.rs crates/flpdf/tests/xref_tests.rs
    git commit -m "feat: build effective xref table without free entries"

### Task 3: Remove reader-side Free filtering and preserve writer Free output

Files:

- Modify crates/flpdf/src/cache.rs:22-39 to make source-cache construction accept only effective live entries.
- Modify crates/flpdf/src/reader/resolver.rs:980-1010 to expose xref refs without a Free predicate.
- Modify crates/flpdf/src/reader.rs:1337-1360,1687-1724 to update live-object documentation and registration.
- Inspect crates/flpdf/src/engine.rs:117-129 and writer xref assembly in crates/flpdf/src/writer.rs.
- Test crates/flpdf/src/cache.rs and crates/flpdf/tests/writer_tests.rs.

- [ ] Step 1: Replace the reader cache's source Free conversion with an unreachable input arm.

Keep CacheEntry::Deleted for explicit Pdf::delete_object, but change ObjectCache::entry_from_xref so source construction has only the two effective reader variants:

    fn entry_from_xref(xref_entry: XrefEntry) -> CacheEntry {
        match xref_entry {
            XrefEntry::Uncompressed { offset } => CacheEntry::Unresolved { offset },
            XrefEntry::Compressed { stream, index } => CacheEntry::Compressed { stream, index },
            XrefEntry::Free { .. } => {
                unreachable!("reader effective xref cannot contain free entries")
            }
        }
    }

- [ ] Step 2: Replace xref_refs_matching use with an all-effective-refs accessor.

Add ResolverHandle::xref_refs that clones only the keys of source_xref_entries. Remove the now-unused predicate accessor and change Pdf::get_all_object_handles to call xref_refs. Rewrite its comments to state that the source effective table already excludes free entries; retain the existing exclusion of explicit cache Deleted, Missing, and Reserved entries in live_object_refs.

- [ ] Step 3: Confirm engine and writer boundaries.

Keep engine.rs's uncompressed-offset collection unchanged: it is a variant selection for writer boundary offsets, not a Free filter. Do not remove XrefEntry::Free, writer free-list construction, or explicit deletion output. Run the existing writer tests that assert Free { next } rows and add no reader Free rows to writer input.

- [ ] Step 4: Run focused consumer tests and commit.

    cargo test -p flpdf cache::tests
    cargo test -p flpdf --test reader_tests
    cargo test -p flpdf --test xref_tests
    cargo test -p flpdf --test writer_tests

Expected result: source readers no longer expose Free rows, explicit deletion and writer free-list tests remain green.

    git add crates/flpdf/src/cache.rs crates/flpdf/src/reader.rs crates/flpdf/src/reader/resolver.rs
    git commit -m "refactor: rely on effective reader xref entries"

### Task 4: Differential verification, coverage, and Beads handoff

Files:

- Verify crates/flpdf/src/xref.rs, crates/flpdf/src/cache.rs, crates/flpdf/src/reader.rs, crates/flpdf/src/reader/resolver.rs, and crates/flpdf/tests/xref_tests.rs.
- Verify docs/superpowers/specs/2026-08-08-effective-xref-free-entry-design.md.

- [ ] Step 1: Run format, lint, and focused compatibility checks.

    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test -p flpdf --test xref_tests
    cargo test -p flpdf --test reader_tests
    cargo test -p flpdf --test writer_tests
    cargo test -p flpdf --test compat_matrix_tests

- [ ] Step 2: Run qpdf live differential probes.

Run the xref differential tests with /usr/bin/qpdf available and inspect qpdf --show-xref for the hybrid fixture, latest-free fixture, and wide-generation type-0 fixture. Confirm qpdf and flpdf both omit free rows and retain the hybrid live row. Confirm qpdf's warning text matches the new /Size diagnostic.

- [ ] Step 3: Run changed-line coverage.

    cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail
    scripts/patch-coverage.sh --base origin/feat/flpdf-25kg.3.32 --lcov

If a changed branch is not covered, add the smallest source-near regression test before claiming completion.

- [ ] Step 4: Run workspace tests and inspect the final diff.

    cargo test
    git diff --check
    git status --short
    git diff --stat 37cfbfde..HEAD

Verify that only the design/plan records and implementation files are present, that writer Free behavior is unchanged, and that no generated fixture or coverage side files are staged.

- [ ] Step 5: Read back Beads state and persist it.

    bd show flpdf-25kg.3.30
    bd dolt push

Do not close the Bead until every acceptance criterion has evidence. At handoff, report the worktree path, branch, commits, test results, qpdf differential result, coverage result, and any remaining dependency boundary.
