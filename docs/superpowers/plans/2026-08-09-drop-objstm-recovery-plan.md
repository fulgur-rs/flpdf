# Drop ObjStm Content Recovery Implementation Plan

> For agentic workers: REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

Goal: Remove flpdf's non-qpdf ObjStm content gap-filler from xref reconstruction while preserving explicit xref type-2 handling.

Architecture: Keep recover_xref_entries as the line-oriented type-1 reconstruction primitive. Remove the gap-filler from recover_xref_from_linear_scan and ResolverCore::reconstruct_xref_and_retry, then let absent packed members use the existing missing/null path. Keep xref table/stream type-2 parsing, XrefEntry::Compressed, and explicit ObjStm parsing untouched.

Tech Stack: Rust workspace, cargo test, qpdf 11.9.0 source at /home/ubuntu/.cache/flpdf/qpdf-11.9.0, /usr/bin/qpdf 11.9.0.

## Global Constraints

- qpdf 11.9.0 source and observed behavior are the semantic oracle.
- Recovery must discover only raw N G obj type-1 entries; it must not inspect ObjStm contents.
- Explicit xref type-2 entries remain a separate responsibility.
- Use RED to GREEN TDD; each changed expectation must fail before production removal.
- Do not add compatibility bridges, sentinels, panics, or new dependencies.
- Preserve main; all implementation changes stay on fix/flpdf-4zt3-drop-objstm-recovery.

---

### Task 1: Make qpdf recovery expectations fail first

Files:
- Modify: crates/flpdf/tests/xref_tests.rs:787-954
- Modify: crates/flpdf/src/reader/resolver.rs tests around 7317-7334 and 7752-7768

Interfaces:
- Consumes: existing best-effort fixtures and recovered_objstm_member_pdf().
- Produces: regression expectations that packed members without explicit type-2 xref entries are absent/null.

- [ ] Step 1: Change the three best-effort positive tests

Keep the assertions that the ObjStm container is XrefEntry::Uncompressed; replace each packed-member Some(XrefEntry::Compressed { ... }) assertion with an absence assertion:

~~~rust
assert_eq!(
    loaded.entries.get(&ObjectRef::new(7, 0)),
    None,
    "reconstruction must not synthesize a type-2 entry from ObjStm contents"
);
~~~

Apply this to best_effort_recovers_objstm_compressed_entries, best_effort_recovers_objstm_with_indirect_length, and best_effort_recovers_objstm_truncated_by_in_stream_header. Update comments so they describe the qpdf boundary rather than proving a fallback.

- [ ] Step 2: Change public resolution expectations

Rename public_resolve_falls_back_to_legacy_for_recovered_compressed_entries to describe an unindexed packed member and assert the existing public null result:

~~~rust
assert_eq!(
    pdf.resolve(ObjectRef::new(7, 0)).expect("absent packed member resolves to null"),
    crate::Object::Null
);
~~~

Rename reconstruction_returns_unsupported_for_recovered_compressed_target and change it to assert that try_dereference() succeeds with a null/absent handle, not Error::Unsupported. Keep explicit type-2 resolver tests that install an xref entry directly.

- [ ] Step 3: Run the changed focused tests and verify RED

Run:

~~~bash
cargo test -p flpdf --test xref_tests best_effort_recovers_objstm -- --nocapture
cargo test -p flpdf --lib recovered_compressed -- --nocapture
~~~

Expected: the new absence/null assertions fail because the current gap-filler still creates XrefEntry::Compressed entries and the legacy fallback resolves them.

---

### Task 2: Remove the non-qpdf gap-filler from both recovery paths

Files:
- Modify: crates/flpdf/src/xref.rs:497-560,639-655,966-1135
- Modify: crates/flpdf/src/reader/resolver.rs:776-805

Interfaces:
- Consumes: recover_xref_entries, candidate xref re-entry, and existing xref entry lookup.
- Produces: qpdf-faithful reconstruction with no synthetic compressed entries.

- [ ] Step 1: Remove the open-time gap-filler call

Delete the recover_objstm_compressed_entries call and its surrounding comments from recover_xref_from_linear_scan. Remove deleted_object_numbers plumbing from that function and recover_trailer_from_xref_stream_candidate if rg confirms it has no remaining consumer.

- [ ] Step 2: Remove the resolve-time gap-filler call

In ResolverCore::reconstruct_xref_and_retry, retain the direct call to recover_xref_entries(logical_bytes), but delete the follow-up gap-filler call and comments. The existing Some(XrefEntry::Compressed) branch remains for explicit entries; a missing reconstructed entry follows Ok(None).

- [ ] Step 3: Remove helper-only production code

After both call sites are gone, remove MAX_OBJSTM_RECOVERY_FALLBACKS, recover_objstm_compressed_entries, try_recover_objstm_in, and recover_compressed_offsets_from_objstm if rg shows no remaining production or test references. Do not remove XrefEntry::Compressed, parse_object_stream_entry, xref-stream parsing, or explicit type-2 consumers.

- [ ] Step 4: Run the focused tests and verify GREEN

Run the two commands from Task 1. Expected: all changed xref and resolver tests pass, with no synthetic type-2 recovery.

---

### Task 3: Remove helper-only tests and repair qpdf-facing documentation

Files:
- Modify: crates/flpdf/src/xref.rs helper-specific tests around 2692-3010
- Modify: crates/flpdf/tests/xref_tests.rs helper-specific comments and fixtures around 956-1086
- Modify: crates/flpdf/src/reader.rs comments around 1811-1842 only if they still claim reconstructed compressed fallback

Interfaces:
- Consumes: the production symbol search from Task 2.
- Produces: tests and docs that describe only qpdf-supported reconstruction behavior.

- [ ] Step 1: Enumerate remaining helper references

Run:

~~~bash
rg -n "recover_objstm_compressed_entries|recover_compressed_offsets_from_objstm|MAX_OBJSTM_RECOVERY_FALLBACKS|gap-filler" crates/flpdf/src crates/flpdf/tests
~~~

- [ ] Step 2: Remove tests whose sole purpose was synthetic recovery

Remove helper-only fallback-budget, malformed-ObjStm insertion-arm, and past-EOF helper tests. Rewrite the xref-stream candidate free-entry test, if still present, to assert that a freed packed member remains absent without claiming that a tombstone blocks a helper that no longer exists. Preserve tests for explicit xref-stream generation, distinct object generations, candidate discovery, and free-entry semantics.

- [ ] Step 3: Update stale qpdf correspondence comments

Change recover_xref_entries documentation to state that it is the complete reconstruction scan and does not have a post-scan ObjStm pass. Remove references to callers needing a gap-filler. Keep qpdf source citations at QPDF.cc:532-575 and QPDF.cc:618-623.

- [ ] Step 4: Run formatting and focused tests

Run:

~~~bash
cargo fmt --all -- --check
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --lib
~~~

Expected: formatting succeeds and all flpdf xref/resolver tests pass.

---

### Task 4: Verify the implementation and hand off the branch

Files:
- Modify: none beyond Tasks 1-3

Interfaces:
- Consumes: the qpdf-faithful implementation and test suite.
- Produces: verified branch state ready for review; flpdf-4zt3 remains open until review/merge policy says otherwise.

- [ ] Step 1: Run the crate quality gates

Run:

~~~bash
cargo test -p flpdf
cargo test -p flpdf-cli
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
~~~

- [ ] Step 2: Inspect the final diff and symbol boundary

Run:

~~~bash
git diff --check
git diff --stat
git status --short
rg -n "recover_objstm_compressed_entries|recover_compressed_offsets_from_objstm|MAX_OBJSTM_RECOVERY_FALLBACKS" crates/flpdf/src crates/flpdf/tests
~~~

The final symbol search may return only historical Beads/docs references; no production call or definition may remain.

- [ ] Step 3: Commit the implementation

~~~bash
git add crates/flpdf/src/xref.rs crates/flpdf/src/reader/resolver.rs crates/flpdf/src/reader.rs crates/flpdf/tests/xref_tests.rs docs/superpowers/specs/2026-08-09-drop-objstm-recovery-design.md docs/superpowers/plans/2026-08-09-drop-objstm-recovery-plan.md
git commit -m "fix: match qpdf xref recovery for ObjStm members"
~~~

- [ ] Step 4: Push Beads and the implementation branch

Run bd dolt push, then git push -u origin fix/flpdf-4zt3-drop-objstm-recovery. Do not close flpdf-4zt3 or merge the branch without a separate approval.
