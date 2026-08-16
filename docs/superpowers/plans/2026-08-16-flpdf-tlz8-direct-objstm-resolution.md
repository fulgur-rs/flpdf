# qpdf Direct Object-Stream Resolution Parity Plan

> For agentic workers: use superpowers:executing-plans or superpowers:subagent-driven-development to execute this plan task by task. Steps use checkbox syntax.

**Goal:** Replace flpdf's lazy compressed-object read route with the qpdf 11.9.0 contract: the type-2 xref field1 selects one direct ObjStm container, that container's header is keyed by object number, and `/Extends` is not traversed during read resolution.

**Architecture:** Keep `XrefEntry::Compressed { stream, index }` as the raw xref identity. In the canonical reader, resolve `stream` directly, parse its `/N` header pairs into an object-number-to-offset map, and materialize only a header object whose effective type-2 xref entry names the same direct stream. Retain `index` only for xref identity, stale-cache/provenance checks, and serialization-facing data; it is not the member selector used by `QPDF::resolveObjectsInStream`. Remove the old `/Extends` chain read bridge after consumer cutover, while leaving writer/rewrite `/Extends` preservation untouched.

**Tech Stack:** Rust workspace, `ObjectHandle`, qpdf 11.9.0 pinned source and `/usr/bin/qpdf` live oracle, existing ObjStm fixtures, `tempfile` and `std::process::Command` differential probes, cargo test/clippy/doc, patch coverage, Beads, and three dependent GitHub Draft PRs.

## Global Constraints

- Work only in `/home/ubuntu/flpdf/.worktrees/flpdf-tlz8-oracle`; preserve `/home/ubuntu/flpdf/main` and all existing worktrees.
- Keep `main` unchanged. Branches are bottom-to-top: `feature/flpdf-tlz8-oracle` -> `feature/flpdf-tlz8-direct-object-number` -> `feature/flpdf-tlz8-chain-cleanup`.
- Follow RED -> GREEN TDD. Add or strengthen a failing test before changing production code for each behavior.
- qpdf 11.9.0 source and fresh qpdf output outrank the current flpdf implementation, prior issue wording, and compatibility with the old chain mechanism.
- `QPDFXRefEntry::getObjStreamIndex()` is not a read member selector in qpdf 11.9.0 `QPDF::resolveObjectsInStream`; do not reintroduce it under another name.
- The read path must not follow `/Extends`. Preserve writer/rewrite `/Extends` handling and its tests unless a separate qpdf oracle proves a writer gap.
- Do not import qtest fixtures from the separate `flpdf-qtest` repository. Use flpdf-authored synthetic fixtures or existing flpdf fixtures.
- Every PR body and comment must describe the remaining stack layers without the phrase that says merge is delegated to another session. Mark a PR Ready only after its required CI checks are green. Do not merge.
- After every edit, run the narrowest relevant tests first. Before any completion claim, run the repository quality gates and capture fresh output.

## qpdf Contract and Existing Gap

Use these pinned source locations in tests, module docs, and PR evidence:

- `include/qpdf/QPDFXRefEntry.hh:45-61`: type-2 entries retain object-stream number and field2 index.
- `libqpdf/QPDF.cc:1729-1833`: `resolve()` dispatches to `resolveObjectsInStream(stream)`. The resolver gets that direct container, reads `/N` and `/First`, builds a header map keyed by object number, verifies the effective xref entry is type-2 for the same direct stream, and reads by header object number. It does not use `getObjStreamIndex()` to choose a member and does not walk `/Extends`.
- `libqpdf/QPDF.cc:1227`: field2 is useful for informational xref display, not read resolution.
- `libqpdf/QPDFWriter.cc:1731-1739,1978`: writer preservation/generation of `/Extends` is a separate route and remains out of scope.
- `crates/flpdf/src/xref.rs:884-960,3072-3081`: bootstrap resolution already models the object-number-keyed direct container contract and raw type-2 storage; keep it as a separate route.
- `crates/flpdf/src/reader.rs:3457-3505,3881-4075`: current canonical resolution discards header object numbers, uses positional `target_index`, and crosses an `/Extends` chain with a global-index calculation.
- `crates/flpdf/src/pdf.rs:12-25`: current provenance documentation describes the old chain meaning and must be corrected during cleanup.

The existing `tests/fixtures/compat/objstm-extends-chain.pdf.hex` is an intentional RED oracle case. With qpdf 11.9.0, object 2 is null and object 3 is 99; the current flpdf reader returns 42 and 99. A second valid fixture must distinguish a child-local direct container value from the parent value, so the implementation cannot pass by merely changing one malformed expectation.

## Task 1: Establish the RED oracle fixture and test surface

**Branch:** `feature/flpdf-tlz8-oracle` based on `origin/main`.

**Files:**

- Add or modify: `crates/flpdf/tests/reader_tests.rs` with an explicit direct-container fixture builder (a committed `.hex` fixture is also acceptable)
- Inspect only: `tests/fixtures/compat/objstm-extends-chain.pdf.hex`, `crates/flpdf/tests/writer_tests.rs`

- [ ] Re-read the existing reader and writer tests around the ObjStm fixtures and preserve the writer `/Extends` assertions as a separate behavior.
- [ ] Build a valid synthetic PDF with a parent ObjStm containing two members and a child ObjStm with `/Extends` pointing at the parent. Give the child a compressed xref entry whose field1 is the child stream and whose field2 would select a different member under the old global-chain interpretation. Include direct xref entries for a parent member and a child member so the test distinguishes source stream from field2.
- [ ] Add a helper that materializes the `.hex` fixture into a `tempfile::TempDir` and a qpdf probe that runs `qpdf --show-object=<n>`. Skip only the external differential assertion when qpdf is unavailable; keep Rust resolution assertions unconditional.
- [ ] Add a test for the valid fixture that records the qpdf child-local and parent values and compares the current flpdf `Pdf::resolve` result. The test must fail against the current chain implementation by observing the parent value for the child object.
- [ ] Turn the existing malformed fixture behavior into an explicit qpdf differential regression: qpdf object 2 is `null`, qpdf object 3 is `99`, while the current flpdf object 2 is `42` and object 3 is `99`. Keep the test diagnostic clear enough to show which side selected the wrong member.
- [ ] Add coverage cases for a header object-number mismatch and an effective xref entry whose source stream differs from the container. These must establish the expected unresolved/null contract before implementation rather than encoding the old positional behavior.
- [ ] Run the RED checks before touching `crates/flpdf/src`:

~~~bash
cargo test -p flpdf --test reader_tests objstm_direct_container_qpdf_contract
cargo test -p flpdf --test reader_tests resolves_compressed_entry_declared_in_extended_object_stream
~~~

Expected result: the new valid/direct-container assertion fails because the current reader crosses `/Extends` and/or selects by positional index. Record the failure and the qpdf output in the PR description.
- [ ] Run the existing writer tests to prove the fixture does not make the writer scope ambiguous:

~~~bash
cargo test -p flpdf --test writer_tests objstm
~~~

- [ ] Commit only the fixture/test layer with a message such as `test(reader): pin qpdf direct object-stream behavior`, then push `feature/flpdf-tlz8-oracle`.
- [ ] Create a Draft PR against `main`. Its body must cite the qpdf source lines, show the RED qpdf/flpdf values, identify the next parser layer, and list the writer scope exclusion. Do not mark it Ready until required CI is green.
- [ ] Request an independent code review of the fixture construction and oracle assertions. Reject any review suggestion that reintroduces `/Extends` traversal or treats field2 as the qpdf read selector without a new source/live-probe justification.
- [ ] After review and CI, update `flpdf-tlz8` notes with the PR number, commit, test commands, review disposition, and CI evidence; run `bd dep cycles` and `bd dolt push`.

## Task 2: Cut over the canonical reader to the direct object-number contract

**Branch:** create `feature/flpdf-tlz8-direct-object-number` from the merged-equivalent tip of Task 1. Keep the Task 1 branch unchanged.

**Files:**

- Modify: `crates/flpdf/src/reader.rs` at `resolve_compressed_entry`, `parse_object_stream_chain_entry`, `parse_object_stream_entry_from_handle`, and compressed provenance helpers
- Inspect/modify as needed: `crates/flpdf/src/reader/resolver.rs` provenance tests, `crates/flpdf/src/subset_prune.rs`, `crates/flpdf/src/pdf.rs`
- Test: `crates/flpdf/tests/reader_tests.rs` and the relevant reader library tests

- [ ] Before implementation, run the failing Task 1 tests and inspect all callers with `rg` so the production route and provenance consumers are explicit.
- [ ] Add a focused unit/integration test for the parser contract before changing its implementation: supply a header with object numbers whose order differs from the xref field2 values and assert that the requested object number, not the positional index, is selected. Add a source-stream mismatch case that must remain unresolved.
- [ ] Change the canonical compressed-entry route so the type-2 field1 directly resolves the one ObjStm container. Do not call `object_stream_chain_member` or `collect_object_stream_chain` from this route.
- [ ] Change the object-stream header parser to retain each `(object_number, offset)` pair in an object-number keyed map. Pass the requested `ObjectRef`/object number to selection; do not pass field2 as a positional target.
- [ ] Before materialization, compare the effective xref entry for the header object with the direct source stream. Only a type-2 entry naming that same stream may be read from the header. A missing header, source-stream mismatch, non-stream container, or malformed header must follow the existing `Result`/known-null behavior established by the oracle tests, not silently fall back to a parent chain.
- [ ] Record compressed provenance with the direct source `stream_ref`. Keep the raw source `(stream,index)` identity needed by `synchronize_legacy_resolution_state` to reject stale caches, but do not claim that `parent_index` came from a global `/Extends` position. Update the consumer only where its meaning is actually used; `subset_prune` needs the parent reference, not an invented chain index.
- [ ] Preserve the already-correct `xref.rs` bootstrap path and writer/rewrite behavior. Add a regression that resolves the same fixture through `Pdf::resolve` and the CLI-facing path if the existing test harness exposes it.
- [ ] Run the RED-to-GREEN sequence:

~~~bash
cargo test -p flpdf --test reader_tests objstm_direct_container_qpdf_contract
cargo test -p flpdf --test reader_tests resolves_compressed_entry_declared_in_extended_object_stream
cargo test -p flpdf --lib compressed_member
cargo test -p flpdf --test cli_tests dump_object
~~~

Expected result: the valid child resolves to the qpdf child-local value, the malformed fixture resolves object 2 as null and object 3 as 99, and no `/Extends` parent is consulted by the lazy read route.
- [ ] Run formatting and the relevant broader suites:

~~~bash
cargo fmt --all -- --check
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --lib
cargo test -p flpdf-cli --test cli_tests
~~~

- [ ] Run an independent code review focused on qpdf source correspondence, effective-xref checks, provenance semantics, and absence of a positional-index fallback. Verify every review claim with pinned source or a live qpdf probe before changing code.
- [ ] Commit the parser/provenance cutover with a message such as `fix(reader): resolve object streams by direct header object`, push the branch, and create the second Draft PR with the first PR as its base.
- [ ] After review and green CI, update `flpdf-tlz8` with PR/commit/check evidence, run `bd dep cycles`, and `bd dolt push`.

## Task 3: Remove the obsolete chain bridge and correct correspondence docs

**Branch:** create `feature/flpdf-tlz8-chain-cleanup` from the Task 2 tip.

**Files:**

- Modify: `crates/flpdf/src/reader.rs`
- Modify: `crates/flpdf/src/pdf.rs` and any module/correspondence documentation that describes `/Extends` as a read-resolution chain
- Inspect/modify: reader resolver tests that manually construct `CompressedMemberProvenance`
- Preserve/test: `crates/flpdf/tests/writer_tests.rs`, `crates/flpdf/src/rewrite_renumber.rs`

- [ ] Run `rg -n "object_stream_chain_member|collect_object_stream_chain|MAX_OBJECT_STREAM_CHAIN_DEPTH|compressed_parent_for_entry|/Extends" crates/flpdf/src crates/flpdf/tests` and classify every remaining hit as canonical read code, provenance consumer, writer/rewrite code, or test/documentation.
- [ ] After the Task 2 cutover has no read callers, remove `object_stream_chain_member`, `collect_object_stream_chain`, the read-only depth/cycle bridge, and any now-unused imports/constants. Do not remove writer/rewrite `/Extends` handling or tests.
- [ ] Update `CompressedMemberProvenance` documentation and `pdf.rs` comments to say that the parent/source is the direct ObjStm named by type-2 field1; field2 remains raw xref provenance, not a global chain member position.
- [ ] Update manual provenance tests to cover direct source identity and stale `(stream,index)` detection. Remove tests whose only contract was preserving the old chain traversal; replace them with direct-container tests where a broken/cyclic `/Extends` parent cannot affect a valid direct child resolution.
- [ ] Re-run the qpdf differential fixture, reader suite, writer ObjStm suite, and CLI dump-object tests. Confirm writer `/Extends` output remains unchanged.
- [ ] Perform a no-callers/no-symbols check and inspect the final diff for accidental changes in `xref.rs` bootstrap or writer code:

~~~bash
rg -n "object_stream_chain_member|collect_object_stream_chain|MAX_OBJECT_STREAM_CHAIN_DEPTH" crates/flpdf/src crates/flpdf/tests
git diff --check
git diff -- crates/flpdf/src/xref.rs crates/flpdf/src/rewrite_renumber.rs crates/flpdf/tests/writer_tests.rs
~~~

- [ ] Run an independent review of the final responsibility boundary. Any suggested compatibility bridge must be tested against qpdf 11.9.0 first and rejected if it preserves the obsolete flpdf-only chain semantics.
- [ ] Commit with a message such as `refactor(reader): remove obsolete object-stream chain bridge`, push the branch, and create the third Draft PR on top of Task 2. Mark each PR Ready only after its own required CI checks are green.
- [ ] Update `flpdf-tlz8` with all three PR numbers, review outcomes, CI checks, no-callers evidence, and the measured qtest/full-survey before/after result; run `bd dep cycles` and `bd dolt push`.

## Task 4: Per-PR quality gates and patch coverage

Run these gates at each stack tip against that PR's actual parent branch, not only at the final tip:

~~~bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links -D rustdoc::invalid_html_tags" cargo doc --workspace --no-deps --document-private-items
cargo test -p flpdf --test reader_tests
cargo test -p flpdf --test xref_tests
cargo test -p flpdf --test writer_tests
cargo test -p flpdf-cli --test cli_tests
cargo test --workspace
CARGO_BUILD_JOBS=2 cargo llvm-cov --workspace --features qpdf-zlib-compat --ignore-run-fail --lcov --output-path /tmp/flpdf-tlz8-lcov.info
CARGO_BUILD_JOBS=2 scripts/patch-coverage.sh --base <actual-parent-branch> --lcov /tmp/flpdf-tlz8-lcov.info
~~~

- [ ] Record exit status and relevant result lines for every command in the corresponding PR/Beads note.
- [ ] If a test or check fails, stop success claims, reproduce the failure in the same stack tip, and use the systematic-debugging workflow before editing.
- [ ] Verify GitHub required checks through `gh pr checks <number>` and inspect failed logs before any retry or fix.

## Task 5: Beads closure and integration handoff

- [ ] Read back `flpdf-tlz8` and `flpdf-25kg.3.1`; confirm the dependency remains `flpdf-25kg.3.1 -> flpdf-tlz8`, no dependency cycle exists, all PR links/check evidence are recorded, and the corrected qpdf contract is present.
- [ ] Run `bd dolt push` and require the exact `Push complete.` result. Do not close the issue until implementation, tests, review, and CI evidence are all present.
- [ ] Once the stack is fully green and every PR is Ready, leave the branches and PRs available for the separately authorized integration worker. This session must not merge them.
- [ ] Before the final report, run `git status --short --branch` in the main worktree and dedicated worktree, verify no unrelated files were changed, and report the exact PR/check/Beads state in Japanese.
